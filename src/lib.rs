//! `ur20-modbus` is a [Rust](https://rust-lang.org) library for communicating
//! with the Modbus TCP fieldbus coupler Weidmüller UR20-FBC-MOD-TCP.
//!
//! # Example:
//!```rust,norun
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

#[macro_use]
extern crate log;

use std::{
    cell::RefCell,
    collections::HashMap,
    io::{Error, ErrorKind},
    net::SocketAddr,
};
use tokio_modbus::{client::Context as Client, prelude::*};
use ur20::{
    ur20_fbc_mod_tcp::Coupler as MbCoupler, ur20_fbc_mod_tcp::*, Address, ChannelValue, ModuleType,
};

type Result<T> = std::result::Result<T, Error>;

/// A Modbus TCP fieldbus coupler implementation.
pub struct Coupler {
    client: Client,
    input_count: u16,
    output_count: u16,
    modules: Vec<ModuleType>,
    coupler: RefCell<MbCoupler>,
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
        debug!("create coupler");
        let cfg = CouplerConfig {
            modules: modules.clone(),
            offsets: raw_offsets,
            params,
        };
        let coupler = MbCoupler::new(&cfg).map_err(|err| Error::new(ErrorKind::Other, err))?;
        Ok(Coupler {
            client: ctx,
            coupler: RefCell::new(coupler),
            input_count,
            output_count,
            modules,
        })
    }

    /// Read the actual coupler ID.
    pub async fn id(&mut self) -> Result<String> {
        debug!("Read the coupler ID");
        let buff = self.client.read_input_registers(ADDR_COUPLER_ID, 7).await?;
        let buf: Vec<u8> = buff.iter().fold(vec![], |mut x, elem| {
            x.push((elem & 0xff) as u8);
            x.push((elem >> 8) as u8);
            x
        });
        let id = String::from_utf8_lossy(&buf).to_string();
        Ok(id)
    }

    /// Current input state.
    pub fn inputs(&self) -> HashMap<Address, ChannelValue> {
        self.coupler
            .borrow()
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
    pub fn outputs(&self) -> HashMap<Address, ChannelValue> {
        self.coupler
            .borrow()
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
    pub fn modules(&self) -> &[ModuleType] {
        // TODO: expose 'modules' in 'ur20' crate
        &self.modules
    }

    /// Set the value of an output channel.
    pub fn set_output(
        &self,
        addr: &Address,
        val: ChannelValue,
    ) -> std::result::Result<(), ur20::Error> {
        self.coupler.borrow_mut().set_output(addr, val)
    }

    async fn input(&mut self) -> Result<Vec<u16>> {
        self.client
            .read_input_registers(ADDR_PACKED_PROCESS_INPUT_DATA, self.input_count)
            .await
    }

    async fn output(&mut self) -> Result<Vec<u16>> {
        self.client
            .read_holding_registers(ADDR_PACKED_PROCESS_OUTPUT_DATA, self.output_count)
            .await
    }

    /// Read binary input data.
    pub fn read_input_data(&self) -> HashMap<Address, Option<Vec<u8>>> {
        let mut map = HashMap::new();
        let mut c = self.coupler.borrow_mut();
        for (i, _) in self.modules.iter().enumerate() {
            if let Some(r) = c.reader(i) {
                let addr = Address {
                    module: i,
                    channel: 0,
                };
                let mut buf = vec![];
                let res = match r.read_to_end(&mut buf) {
                    Ok(len) => {
                        if len > 0 {
                            Some(buf)
                        } else {
                            None
                        }
                    }
                    Err(_) => {
                        // Should never happen: see ur20 crate
                        debug_assert!(false);
                        None
                    }
                };
                map.insert(addr, res);
            }
        }
        map
    }

    fn next_out(&mut self, input: &[u16], output: &[u16]) -> Result<Vec<u16>> {
        self.coupler
            .borrow_mut()
            .next(&input, &output)
            .map_err(|err| Error::new(ErrorKind::Other, err))
    }

    async fn write(&mut self, output: &[u16]) -> Result<()> {
        self.client
            .write_multiple_registers(ADDR_PACKED_PROCESS_OUTPUT_DATA, output)
            .await
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
        debug!("fetch data");
        let (input, output) = self.get_data().await?;
        let output = self.next_out(&input, &output)?;
        debug!("write data");
        self.write(&output).await?;
        Ok(())
    }

    /// This method behaves like [Coupler::tick] but it consumes the [Coupler] and returns it.
    pub async fn tick_and_consume(mut self) -> Result<Self> {
        debug!("fetch data");
        let (input, output) = self.get_data().await?;
        let output = self.next_out(&input, &output)?;
        debug!("write data");
        self.write(&output).await?;
        Ok(self)
    }
}

async fn read_module_count(client: &mut Client) -> Result<u16> {
    debug!("Read module count");
    let buff = client
        .read_input_registers(ADDR_CURRENT_MODULE_COUNT, 1)
        .await?;
    if buff.is_empty() {
        return Err(Error::new(ErrorKind::Other, "Invalid buffer length"));
    }
    let cnt = buff[0];
    if cnt == 0 {
        warn!("UR20-System has no modules!");
    }
    Ok(cnt)
}

async fn read_module_list(client: &mut Client, cnt: u16) -> Result<Vec<ModuleType>> {
    if cnt == 0 {
        // TODO
    }
    let raw_list = client
        .read_input_registers(ADDR_CURRENT_MODULE_LIST, cnt * 2)
        .await?;
    let module_list =
        module_list_from_registers(&raw_list).map_err(|err| Error::new(ErrorKind::Other, err))?;
    Ok(module_list)
}

fn print_module_list_info(module_list: &[ModuleType]) {
    let module_names: Vec<_> = module_list
        .iter()
        .map(|m| format!("{:?}", m))
        .map(|n| n.replace("UR20_", ""))
        .collect();
    info!("The following I/O modules were detected:");
    for (i, n) in module_names.iter().enumerate() {
        info!(" {} - {}", i, n);
    }
}

async fn read_module_offsets(client: &mut Client, module_list: &[ModuleType]) -> Result<Vec<u16>> {
    debug!("read module offsets");
    client
        .read_input_registers(ADDR_MODULE_OFFSETS, module_list.len() as u16 * 2)
        .await
}

async fn read_process_input_register_count(client: &mut Client) -> Result<u16> {
    debug!("read process input length");
    let raw_input_count = client
        .read_input_registers(ADDR_PROCESS_INPUT_LEN, 1)
        .await?;
    let cnt = if raw_input_count.is_empty() {
        0
    } else {
        raw_input_count[0]
    };
    let input_count = process_img_len_to_register_count(cnt);
    Ok(input_count)
}

async fn read_process_output_register_count(client: &mut Client) -> Result<u16> {
    debug!("read process output length");
    let raw_output_count = client
        .read_input_registers(ADDR_PROCESS_OUTPUT_LEN, 1)
        .await?;
    let cnt = if raw_output_count.is_empty() {
        0
    } else {
        raw_output_count[0]
    };
    let output_count = process_img_len_to_register_count(cnt);
    Ok(output_count)
}

async fn read_parameters(client: &mut Client, module_list: &[ModuleType]) -> Result<Vec<Vec<u16>>> {
    debug!("read parameters");
    let mut params = vec![];
    for (addr, reg_cnt) in &param_addresses_and_register_counts(&module_list) {
        params.push(if *reg_cnt == 0 {
            vec![]
        } else {
            client.read_holding_registers(*addr, *reg_cnt).await?
        })
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
