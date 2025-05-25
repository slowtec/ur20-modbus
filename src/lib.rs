//! `ur20-modbus` is a [Rust](https://rust-lang.org) library for communicating
//! with the Modbus TCP fieldbus coupler Weidmüller UR20-FBC-MOD-TCP.
//!
//! # Example:
//!```rust,no_run
//! use std::error::Error;
//! use ur20_modbus::Coupler;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn Error>> {
//!     let addr = "192.168.0.222:502".parse()?;
//!     let mut coupler = Coupler::connect(addr).await?;
//!     let id = coupler.id().await?;
//!     println!("Connected to {}", id);
//!     coupler.set_output(
//!         &ur20::Address {
//!             module: 3,
//!             channel: 2,
//!         },
//!         ur20::ChannelValue::Bit(true),
//!     )?;
//!     coupler.tick().await?;
//!     Ok(())
//! }
//!```

use std::{collections::HashMap, io, net::SocketAddr};

use tokio_modbus::{
    client::{Client as _, Context as Client},
    prelude::*,
};

use ur20::{
    Address, ChannelValue, ModuleType, ur20_fbc_mod_tcp::Coupler as MbCoupler, ur20_fbc_mod_tcp::*,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    IoError(#[from] io::Error),
    #[error(transparent)]
    ModbusError(#[from] tokio_modbus::Error),
    #[error(transparent)]
    ModbusException(#[from] tokio_modbus::ExceptionCode),
    #[error(transparent)]
    Ur20Error(#[from] ur20::Error),
    #[error("Unexpected response: {0}")]
    UnexpectedResponse(String),
}

type Result<T> = std::result::Result<T, Error>;

/// A Modbus TCP fieldbus coupler implementation.
pub struct Coupler {
    client: Client,
    input_count: u16,
    output_count: u16,
    modules: Vec<ModuleType>,
    coupler: MbCoupler,
}

impl Coupler {
    /// Connect to the coupler.
    pub async fn connect(addr: SocketAddr) -> Result<Coupler> {
        let mut ctx = tcp::connect(addr).await?;
        let cnt = read_module_count(&mut ctx).await?;
        let modules = read_module_list(&mut ctx, cnt).await?;
        print_module_list_info(&modules);
        let raw_offsets = read_module_offsets(&mut ctx, &modules).await?;
        let input_count = read_process_input_register_count(&mut ctx).await?;
        let output_count = read_process_output_register_count(&mut ctx).await?;
        let params = read_parameters(&mut ctx, &modules).await?;
        log::debug!("create coupler");
        let cfg = CouplerConfig {
            modules: modules.clone(),
            offsets: raw_offsets,
            params,
        };
        let coupler = MbCoupler::new(&cfg)?;
        Ok(Coupler {
            client: ctx,
            coupler,
            input_count,
            output_count,
            modules,
        })
    }
    /// Disconnect the coupler.
    pub async fn disconnect(&mut self) -> Result<()> {
        Ok(self.client.disconnect().await?)
    }
    /// Read the actual coupler ID.
    pub async fn id(&mut self) -> Result<String> {
        log::debug!("Read the coupler ID");
        let buff = self
            .client
            .read_input_registers(ADDR_COUPLER_ID, 7)
            .await??;
        let buf: Vec<u8> = buff.iter().fold(vec![], |mut x, elem| {
            x.push((elem & 0xff) as u8);
            x.push((elem >> 8) as u8);
            x
        });
        let id = String::from_utf8_lossy(&buf).to_string();
        Ok(id)
    }

    /// Current input state.
    #[must_use]
    pub fn inputs(&self) -> HashMap<Address, ChannelValue> {
        self.coupler
            .inputs()
            .clone()
            .into_iter()
            .enumerate()
            .flat_map(|(module, vals)| {
                vals.into_iter()
                    .enumerate()
                    .map(move |(channel, value)| (Address { module, channel }, value))
            })
            .collect()
    }

    /// Current output state.
    #[must_use]
    pub fn outputs(&self) -> HashMap<Address, ChannelValue> {
        self.coupler
            .outputs()
            .clone()
            .into_iter()
            .enumerate()
            .flat_map(|(module, vals)| {
                vals.into_iter()
                    .enumerate()
                    .map(move |(channel, value)| (Address { module, channel }, value))
            })
            .collect()
    }

    /// List of modules.
    #[must_use]
    pub fn modules(&self) -> &[ModuleType] {
        // TODO: expose 'modules' in 'ur20' crate
        &self.modules
    }

    /// Set the value of an output channel.
    pub fn set_output(
        &mut self,
        addr: &Address,
        val: ChannelValue,
    ) -> std::result::Result<(), ur20::Error> {
        self.coupler.set_output(addr, val)
    }

    async fn input(&mut self) -> Result<Vec<u16>> {
        if self.input_count == 0 {
            Ok(vec![])
        } else {
            self.client
                .read_input_registers(ADDR_PACKED_PROCESS_INPUT_DATA, self.input_count)
                .await?
                .map_err(Error::ModbusException)
        }
    }

    async fn output(&mut self) -> Result<Vec<u16>> {
        if self.output_count == 0 {
            Ok(vec![])
        } else {
            self.client
                .read_holding_registers(ADDR_PACKED_PROCESS_OUTPUT_DATA, self.output_count)
                .await?
                .map_err(Error::ModbusException)
        }
    }

    /// Read binary input data.
    pub fn binary_input_data(&mut self) -> HashMap<Address, Option<Vec<u8>>> {
        let mut map = HashMap::new();
        for (i, _) in self.modules.iter().enumerate() {
            if let Some(r) = self.coupler.reader(i) {
                let addr = Address {
                    module: i,
                    channel: 0,
                };
                let mut buf = vec![];
                let res = if let Ok(len) = r.read_to_end(&mut buf) {
                    if len > 0 { Some(buf) } else { None }
                } else {
                    // Should never happen: see ur20 crate
                    debug_assert!(false);
                    None
                };
                map.insert(addr, res);
            }
        }
        map
    }

    fn next_out(&mut self, input: &[u16], output: &[u16]) -> Result<Vec<u16>> {
        self.coupler.next(input, output).map_err(Error::Ur20Error)
    }

    async fn write(&mut self, output: &[u16]) -> Result<()> {
        self.client
            .write_multiple_registers(ADDR_PACKED_PROCESS_OUTPUT_DATA, output)
            .await?
            .map_err(Error::ModbusException)
    }

    async fn get_data(&mut self) -> Result<(Vec<u16>, Vec<u16>)> {
        let input = self.input().await?;
        let output = self.output().await?;
        Ok((input, output))
    }

    /// Run an I/O cycle.
    /// This reads all process input registers and
    /// writes to process output registers.
    pub async fn tick(&mut self) -> Result<()> {
        log::debug!("fetch data");
        let (input, output) = self.get_data().await?;
        let output = self.next_out(&input, &output)?;
        log::debug!("write data");
        self.write(&output).await?;
        Ok(())
    }
}

async fn read_module_count(client: &mut Client) -> Result<u16> {
    log::debug!("Read module count");
    let buff = client
        .read_input_registers(ADDR_CURRENT_MODULE_COUNT, 1)
        .await??;
    if buff.is_empty() {
        return Err(Error::UnexpectedResponse("Invalid buffer length".into()));
    }
    let cnt = buff[0];
    if cnt == 0 {
        log::warn!("UR20-System has no modules!");
    }
    Ok(cnt)
}

async fn read_module_list(client: &mut Client, cnt: u16) -> Result<Vec<ModuleType>> {
    if cnt == 0 {
        // TODO
    }
    let raw_list = client
        .read_input_registers(ADDR_CURRENT_MODULE_LIST, cnt * 2)
        .await??;
    let module_list = module_list_from_registers(&raw_list)?;
    Ok(module_list)
}

fn print_module_list_info(module_list: &[ModuleType]) {
    let module_names: Vec<_> = module_list
        .iter()
        .map(|m| format!("{m:?}"))
        .map(|n| n.replace("UR20_", ""))
        .collect();
    log::info!("The following I/O modules were detected:");
    for (i, n) in module_names.iter().enumerate() {
        log::info!(" {i} - {n}");
    }
}

async fn read_module_offsets(client: &mut Client, module_list: &[ModuleType]) -> Result<Vec<u16>> {
    log::debug!("read module offsets");
    client
        .read_input_registers(ADDR_MODULE_OFFSETS, module_list.len() as u16 * 2)
        .await?
        .map_err(Error::ModbusException)
}

async fn read_process_input_register_count(client: &mut Client) -> Result<u16> {
    log::debug!("read process input length");
    let raw_input_count = client
        .read_input_registers(ADDR_PROCESS_INPUT_LEN, 1)
        .await??;
    let cnt = if raw_input_count.is_empty() {
        0
    } else {
        raw_input_count[0]
    };
    let input_count = process_img_len_to_register_count(cnt);
    Ok(input_count)
}

async fn read_process_output_register_count(client: &mut Client) -> Result<u16> {
    log::debug!("read process output length");
    let raw_output_count = client
        .read_input_registers(ADDR_PROCESS_OUTPUT_LEN, 1)
        .await??;
    let cnt = if raw_output_count.is_empty() {
        0
    } else {
        raw_output_count[0]
    };
    let output_count = process_img_len_to_register_count(cnt);
    Ok(output_count)
}

async fn read_parameters(client: &mut Client, module_list: &[ModuleType]) -> Result<Vec<Vec<u16>>> {
    log::debug!("read parameters");
    let mut params = vec![];
    for (addr, reg_cnt) in &param_addresses_and_register_counts(module_list) {
        params.push(if *reg_cnt == 0 {
            vec![]
        } else {
            client.read_holding_registers(*addr, *reg_cnt).await??
        });
    }

    Ok(params)
}

fn process_img_len_to_register_count(process_len: u16) -> u16 {
    let byte_count = if process_len % 8 != 0 {
        (process_len / 8) + 1
    } else {
        process_len / 8
    };
    if byte_count % 2 != 0 {
        (byte_count / 2) + 1
    } else {
        byte_count / 2
    }
}
