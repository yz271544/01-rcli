use anyhow::anyhow;
use rand::seq::SliceRandom;

const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
const NUMBER: &[u8] = b"123456789";
const SYMBOL: &[u8] = b"!@#$%^&*_";

pub fn process_genpass(
    length: u8,
    upper: bool,
    lower: bool,
    number: bool,
    symbol: bool,
) -> anyhow::Result<String> {
    let mut rng = rand::thread_rng();
    let mut password = Vec::new();
    let mut chars = Vec::new();

    if upper {
        chars.extend_from_slice(UPPER);
        password.push(*UPPER.choose(&mut rng).expect("UPPER is non-empty"));
    }
    if lower {
        chars.extend_from_slice(LOWER);
        password.push(*LOWER.choose(&mut rng).expect("LOWER is non-empty"));
    }
    if number {
        chars.extend_from_slice(NUMBER);
        password.push(*NUMBER.choose(&mut rng).expect("NUMBER is non-empty"));
    }
    if symbol {
        chars.extend_from_slice(SYMBOL);
        password.push(*SYMBOL.choose(&mut rng).expect("SYMBOL is non-empty"));
    }

    if chars.is_empty() {
        return Err(anyhow!("at least one character set must be enabled"));
    }

    for _ in 0..(length - password.len() as u8) {
        let c = chars
            .choose(&mut rng)
            .ok_or_else(|| anyhow!("no characters available to sample"))?;
        password.push(*c);
    }

    password.shuffle(&mut rng);

    Ok(String::from_utf8(password)?)
}
