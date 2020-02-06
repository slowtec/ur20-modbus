use std::error::Error;
use ur20_modbus::Coupler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let addr = "192.168.0.222:502".parse()?;
    let mut coupler = Coupler::connect(addr).await?;
    let id = coupler.id().await?;
    println!("Connected to {}", id);
    coupler.set_output(
        &ur20::Address {
            module: 3,
            channel: 2,
        },
        ur20::ChannelValue::Bit(true),
    )?;
    coupler.tick().await?;
    Ok(())
}
