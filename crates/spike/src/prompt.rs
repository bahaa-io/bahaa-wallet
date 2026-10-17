use std::io::{self, BufRead, Write};

use anyhow::Result;

pub fn confirm() -> Result<bool> {
    print!("type yes to broadcast: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim() == "yes")
}
