use tileception_auth::{SecretString, check_password};
use zeroize::Zeroizing;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let user = arguments.next().ok_or("usage: check USER")?;
    if arguments.next().is_some() {
        return Err("usage: check USER (password is read interactively)".into());
    }
    eprint!("Password: ");
    let password = Zeroizing::new(rpassword::read_password()?);
    let secret = SecretString::from(password.as_str());
    drop(password);
    println!("{:?}", check_password(&user, secret)?);
    Ok(())
}
