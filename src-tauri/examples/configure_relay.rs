// Operator setup utility: accepts secret JSON on stdin, never command-line arguments.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;
    let mut input = zeroize::Zeroizing::new(String::new());
    std::io::stdin().take(4097).read_to_string(&mut input)?;
    if input.len() > 4096 {
        return Err("Setup input too large".into());
    }
    m_dash_lib::mobile_sync::configure_relay(&input).map_err(std::io::Error::other)?;
    println!("Relay endpoint and enrollment access saved in this computer's secure storage.");
    Ok(())
}
