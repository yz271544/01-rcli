use crate::CmdExecutor;
use clap::Parser;
use zxcvbn::zxcvbn;

#[derive(Debug, Parser)]
pub struct GenPassOpts {
    #[arg(short, long, default_value_t = 16)]
    pub length: u8,

    #[arg(long = "no-uppercase", action = clap::ArgAction::SetFalse)]
    pub uppercase: bool,

    #[arg(long = "no-lowercase", action = clap::ArgAction::SetFalse)]
    pub lowercase: bool,

    #[arg(long = "no-number", action = clap::ArgAction::SetFalse)]
    pub number: bool,

    #[arg(long = "no-symbol", action = clap::ArgAction::SetFalse)]
    pub symbol: bool,
}

impl CmdExecutor for GenPassOpts {
    async fn execute(self) -> anyhow::Result<()> {
        let ret = crate::process_genpass(
            self.length,
            self.uppercase,
            self.lowercase,
            self.number,
            self.symbol,
        )?;
        println!("{}", ret);

        // output password strength in stderr
        let estimate = zxcvbn(&ret, &[])?;
        eprintln!("Password strength: {}", estimate.score());
        Ok(())
    }
}
