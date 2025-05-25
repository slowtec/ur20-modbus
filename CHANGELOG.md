# Changelog

## v0.5.0 (2025-05-25)

- Update `tokio` to `v1`
- Update `tokio-modbus` to `v0.16`
- Update `ur20` to `v0.6`
- Switch to Rust Edition 2024

## v0.4.0 (2020-03-13)

- Update to `tokio-modbus` `v0.4`
- Remove usage of RefCell
- Remove `tick_and_comsume` method
- Rename fn `read_input_data` to `binary_input_data`

## v0.3.0 (2019-04-02)

- Update to `tokio-modbus` `v0.3`

## v0.2.0 (2018-09-11)

- Update to `ur20` `v0.5`
- Reexport `ur20` crate
- Avoid reading zero registers

## v0.1.2 (2018-06-18)

- Add tick method that consumes the coupler
- Small refactoring

## v0.1.1 (2018-04-24)

- Update to `ur20` `v0.4`
- Improve documentation
- Add method to read collected input bytes

## v0.1.0 (2018-04-16)

- Initial implementation
