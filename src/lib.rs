#[macro_use]
extern crate log;
extern crate futures;
extern crate tokio_core;
extern crate tokio_modbus;
extern crate ur20;

use std::{io::{Error, ErrorKind}, net::SocketAddr};
use futures::future::{self, Future};
use tokio_modbus::*;
use tokio_core::reactor::Handle;
use ur20::{ModuleType, ur20_fbc_mod_tcp::*, ur20_fbc_mod_tcp::Coupler as MbCoupler};

pub use ur20::{Address, ChannelValue};

pub struct Coupler {
    client: Client,
    input_count: u16,
    output_count: u16,
    modules: Vec<ModuleType>,
    coupler: MbCoupler,
}

impl Coupler {
    pub fn connect(addr: SocketAddr, handle: Handle) -> impl Future<Item = Coupler, Error = Error> {
        let coupler = Client::connect_tcp(&addr, &handle)
            .and_then(|client| {
                debug!("Read the coupler ID");
                client
                    .read_input_registers(ADDR_COUPLER_ID, 7)
                    .and_then(move |buff| {
                        let buf: Vec<u8> = buff.iter().fold(vec![], |mut x, elem| {
                            x.push((elem & 0xff) as u8);
                            x.push((elem >> 8) as u8);
                            x
                        });
                        let id = String::from_utf8_lossy(&buf);
                        info!("Connected to coupler '{}'", id);
                        Ok(client)
                    })
            })
            .and_then(|client| {
                debug!("Read module count");
                client
                    .read_input_registers(ADDR_CURRENT_MODULE_COUNT, 1)
                    .and_then(move |buff| {
                        if buff.is_empty() {
                            return Err(Error::new(ErrorKind::Other, "Invalid buffer length"));
                        }
                        let cnt = buff[0];
                        if cnt == 0 {
                            warn!("UR20-System has no modules!");
                        }
                        Ok((client, cnt))
                    })
            })
            .and_then(|(client, cnt)| {
                if cnt == 0 {
                    // TODO
                }
                debug!("Read module list");
                client
                    .read_input_registers(ADDR_CURRENT_MODULE_LIST, cnt * 2)
                    .and_then(move |raw_list| {
                        let module_list = module_list_from_registers(&raw_list)
                            .map_err(|err| Error::new(ErrorKind::Other, err))?;
                        let module_names: Vec<_> = module_list
                            .iter()
                            .map(|m| format!("{:?}", m))
                            .map(|n| n.replace("UR20_", ""))
                            .collect();
                        info!("The following I/O modules were detected:");
                        for (i, n) in module_names.iter().enumerate() {
                            info!(" {} - {}", i, n);
                        }
                        Ok((client, module_list))
                    })
            })
            .and_then(|(client, module_list)| {
                debug!("read module offsets");
                client
                    .read_input_registers(ADDR_MODULE_OFFSETS, module_list.len() as u16 * 2)
                    .and_then(move |raw_offsets| Ok((client, module_list, raw_offsets)))
            })
            .and_then(|(client, module_list, offsets)| {
                debug!("read process input length");
                client
                    .read_input_registers(ADDR_PROCESS_INPUT_LEN, 1)
                    .and_then(move |raw_input_count| {
                        let cnt = if raw_input_count.is_empty() {
                            0
                        } else {
                            raw_input_count[0]
                        };
                        let input_count = process_img_len_to_register_count(cnt);
                        Ok((client, module_list, offsets, input_count))
                    })
            })
            .and_then(|(client, module_list, offsets, input_count)| {
                debug!("read process output length");
                client
                    .read_input_registers(ADDR_PROCESS_OUTPUT_LEN, 1)
                    .and_then(move |raw_output_count| {
                        let cnt = if raw_output_count.is_empty() {
                            0
                        } else {
                            raw_output_count[0]
                        };
                        let output_count = process_img_len_to_register_count(cnt);
                        Ok((client, module_list, offsets, input_count, output_count))
                    })
            })
            .and_then(
                |(client, module_list, offsets, input_count, output_count)| {
                    debug!("read parameters");
                    let params: Vec<_> = param_addresses_and_register_counts(&module_list)
                        .into_iter()
                        .map(|(addr, reg_cnt)| client.read_holding_registers(addr, reg_cnt))
                        .collect();
                    futures::future::join_all(params).and_then(move |params| {
                        Ok((
                            client,
                            module_list,
                            offsets,
                            input_count,
                            output_count,
                            params,
                        ))
                    })
                },
            )
            .and_then(
                |(client, modules, offsets, input_count, output_count, params)| {
                    debug!("create coupler");
                    let cfg = CouplerConfig {
                        modules: modules.clone(),
                        offsets,
                        params,
                    };
                    let coupler =
                        MbCoupler::new(&cfg).map_err(|err| Error::new(ErrorKind::Other, err))?;
                    Ok(Coupler {
                        client,
                        coupler,
                        input_count,
                        output_count,
                        modules,
                    })
                },
            );
        coupler
    }

    pub fn inputs(&self) -> &Vec<Vec<ChannelValue>> {
        self.coupler.inputs()
    }

    pub fn outputs(&self) -> &Vec<Vec<ChannelValue>> {
        self.coupler.outputs()
    }

    pub fn modules(&self) -> &Vec<ModuleType> {
        // TODO: expose 'modules' in 'ur20' crate
        &self.modules
    }

    pub fn set_output(&mut self, addr: &Address, val: ChannelValue) -> Result<(), ur20::Error> {
        self.coupler.set_output(addr, val)
    }

    fn input(&self) -> impl Future<Item = Vec<u16>, Error = Error> {
        self.client
            .read_input_registers(ADDR_PACKED_PROCESS_INPUT_DATA, self.input_count)
    }

    fn output(&self) -> impl Future<Item = Vec<u16>, Error = Error> {
        self.client
            .read_holding_registers(ADDR_PACKED_PROCESS_OUTPUT_DATA, self.output_count)
    }

    fn next_out(
        &mut self,
        input: &[u16],
        output: &[u16],
    ) -> impl Future<Item = Vec<u16>, Error = Error> {
        match self.coupler.next(&input, &output) {
            Ok(data) => future::ok(data),
            Err(err) => future::err(Error::new(ErrorKind::Other, err)),
        }
    }

    fn write(&self, output: &[u16]) -> impl Future<Item = (), Error = Error> {
        self.client
            .write_multiple_registers(ADDR_PACKED_PROCESS_OUTPUT_DATA, output)
    }

    fn get_data(&self) -> impl Future<Item = (Vec<u16>, Vec<u16>), Error = Error> {
        self.input().join(self.output())
    }

    pub fn tick(mut self) -> impl Future<Item = Self, Error = Error> {
        debug!("fetch data");
        self.get_data().and_then(move |(input, output)| {
            self.next_out(&input, &output).and_then(move |output| {
                debug!("write data");
                self.write(&output).and_then(|_| Ok(self))
            })
        })
    }
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
