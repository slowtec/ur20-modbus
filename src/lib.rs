#[macro_use]
extern crate log;
extern crate futures;
extern crate tokio_core;
extern crate tokio_modbus;
extern crate ur20;

use futures::future::{self, Future};
use std::{cell::RefCell,
          collections::HashMap,
          io::{Error, ErrorKind},
          net::SocketAddr};
use tokio_core::reactor::Handle;
use tokio_modbus::*;
use ur20::{ur20_fbc_mod_tcp::Coupler as MbCoupler, ur20_fbc_mod_tcp::*, ModuleType};

pub use ur20::{Address, ChannelValue};

/// A Modbus TCP fieldbus coupler (`UR20-FBC-MOD-TCP`) implementation.
///
/// # Example:
///```rust,no_run
/// extern crate futures;
/// extern crate tokio_core;
/// extern crate ur20_modbus;
///
/// use futures::Future;
/// use tokio_core::reactor::Core;
/// use ur20_modbus::Coupler;
///
/// let mut core = Core::new().unwrap();
/// let handle = core.handle();
/// let addr = "192.168.178.3:502".parse().unwrap();
/// let client = Coupler::connect(addr, handle);
/// let task = client.and_then(|client|{
///     println!("Connected to {}", client.id());
///     Ok(())
/// });
/// core.run(task).unwrap();
///```
pub struct Coupler {
    id: String,
    client: Client,
    input_count: u16,
    output_count: u16,
    modules: Vec<ModuleType>,
    coupler: RefCell<MbCoupler>,
}

impl Coupler {
    /// Connect to the coupler.
    pub fn connect(addr: SocketAddr, handle: Handle) -> impl Future<Item = Coupler, Error = Error> {
        let coupler = Client::connect_tcp(&addr, &handle)
            .and_then(|client| {
                let coupler_id = read_coupler_id(&client);
                let cnt = read_module_count(&client);

                coupler_id.join(cnt).and_then(|(id, cnt)| {
                    read_module_list(&client, cnt)
                        .and_then(|module_list| {
                            print_module_list(&module_list);
                            Ok((client, id, module_list))
                        })
                        .and_then(|(client, id, module_list)| {
                            read_module_offsets(&client, &module_list)
                                .and_then(|raw_offsets| Ok((client, id, module_list, raw_offsets)))
                        })
                        .and_then(|(client, id, module_list, offsets)| {
                            read_process_input_register_count(&client).and_then(|input_count| {
                                Ok((client, id, module_list, offsets, input_count))
                            })
                        })
                        .and_then(|(client, id, module_list, offsets, input_count)| {
                            read_process_output_register_count(&client).and_then(
                                move |output_count| {
                                    Ok((
                                        client,
                                        id,
                                        module_list,
                                        offsets,
                                        input_count,
                                        output_count,
                                    ))
                                },
                            )
                        })
                        .and_then(
                            |(client, id, module_list, offsets, input_count, output_count)| {
                                read_parameters(&client, &module_list).and_then(move |params| {
                                    Ok((
                                        client,
                                        id,
                                        module_list,
                                        offsets,
                                        input_count,
                                        output_count,
                                        params,
                                    ))
                                })
                            },
                        )
                })
            })
            .and_then(
                |(client, id, modules, offsets, input_count, output_count, params)| {
                    debug!("create coupler");
                    let cfg = CouplerConfig {
                        modules: modules.clone(),
                        offsets,
                        params,
                    };
                    let coupler =
                        MbCoupler::new(&cfg).map_err(|err| Error::new(ErrorKind::Other, err))?;
                    Ok(Coupler {
                        id,
                        client,
                        coupler: RefCell::new(coupler),
                        input_count,
                        output_count,
                        modules,
                    })
                },
            );
        coupler
    }

    /// The actual coupler ID.
    pub fn id(&self) -> &str {
        &self.id
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
    pub fn set_output(&self, addr: &Address, val: ChannelValue) -> Result<(), ur20::Error> {
        self.coupler.borrow_mut().set_output(addr, val)
    }

    fn input(&self) -> impl Future<Item = Vec<u16>, Error = Error> {
        self.client
            .read_input_registers(ADDR_PACKED_PROCESS_INPUT_DATA, self.input_count)
    }

    fn output(&self) -> impl Future<Item = Vec<u16>, Error = Error> {
        self.client
            .read_holding_registers(ADDR_PACKED_PROCESS_OUTPUT_DATA, self.output_count)
    }

    /// Read binary input data.
    pub fn read_input_data(&self) -> HashMap<Address, Option<Vec<u8>>> {
        let mut map = HashMap::new();
        let mut c = self.coupler.borrow_mut();
        for (i, _) in self.modules.iter().enumerate() {
            if let Some(r) = c.reader(i) {
                let addr = Address{module: i, channel: 0};
                let mut buf = vec![];
                let res = match r.read_to_end(&mut buf) {
                    Ok(len) => {
                        if len > 0 {
                            Some(buf)
                        } else {
                            None
                        }
                    }
                    Err(_) => None // Should never happen: see ur20 crate
                };
                map.insert(addr, res);
            }
        }
        map
    }

    fn next_out(
        &self,
        input: &[u16],
        output: &[u16],
    ) -> impl Future<Item = Vec<u16>, Error = Error> {
        match self.coupler.borrow_mut().next(&input, &output) {
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

    /// Run an I/O cycle.
    /// This reads all process input registers and
    /// writes to process output registers.
    pub fn tick<'a>(&'a self) -> impl Future<Item = (), Error = Error> + 'a {
        debug!("fetch data");
        self.get_data().and_then(move |(input, output)| {
            self.next_out(&input, &output).and_then(move |output| {
                debug!("write data");
                self.write(&output).and_then(|_| Ok(()))
            })
        })
    }

    /// This method behaves like [Coupler::tick] but it consumes the [Coupler] and returns it as [Future::Item].
    pub fn tick_and_consume(self) -> impl Future<Item = Self, Error = Error> {
        debug!("fetch data");
        self.get_data().and_then(move |(input, output)| {
            self.next_out(&input, &output).and_then(move |output| {
                debug!("write data");
                self.write(&output).and_then(|_| Ok(self))
            })
        })
    }
}

fn read_coupler_id(client: &Client) -> impl Future<Item = String, Error = Error> {
    debug!("Read the coupler ID");
    client
        .read_input_registers(ADDR_COUPLER_ID, 7)
        .and_then(move |buff| {
            let buf: Vec<u8> = buff.iter().fold(vec![], |mut x, elem| {
                x.push((elem & 0xff) as u8);
                x.push((elem >> 8) as u8);
                x
            });
            let id = String::from_utf8_lossy(&buf).to_string();
            Ok(id)
        })
}

fn read_module_count(client: &Client) -> impl Future<Item = u16, Error = Error> {
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
            Ok(cnt)
        })
}

fn read_module_list(
    client: &Client,
    cnt: u16,
) -> impl Future<Item = Vec<ModuleType>, Error = Error> {
    if cnt == 0 {
        // TODO
    }
    client
        .read_input_registers(ADDR_CURRENT_MODULE_LIST, cnt * 2)
        .and_then(move |raw_list| {
            let module_list = module_list_from_registers(&raw_list)
                .map_err(|err| Error::new(ErrorKind::Other, err))?;
            Ok(module_list)
        })
}

fn print_module_list(module_list: &[ModuleType]) {
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

fn read_module_offsets(
    client: &Client,
    module_list: &[ModuleType],
) -> impl Future<Item = Vec<u16>, Error = Error> {
    debug!("read module offsets");
    client
        .read_input_registers(ADDR_MODULE_OFFSETS, module_list.len() as u16 * 2)
        .and_then(move |raw_offsets| Ok(raw_offsets))
}

fn read_process_input_register_count(client: &Client) -> impl Future<Item = u16, Error = Error> {
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
            Ok(input_count)
        })
}

fn read_process_output_register_count(client: &Client) -> impl Future<Item = u16, Error = Error> {
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
            Ok(output_count)
        })
}

fn read_parameters(
    client: &Client,
    module_list: &[ModuleType],
) -> impl Future<Item = Vec<Vec<u16>>, Error = Error> {
    debug!("read parameters");
    let params: Vec<_> = param_addresses_and_register_counts(&module_list)
        .into_iter()
        .map(|(addr, reg_cnt)| client.read_holding_registers(addr, reg_cnt))
        .collect();
    futures::future::join_all(params).and_then(move |params| Ok(params))
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
