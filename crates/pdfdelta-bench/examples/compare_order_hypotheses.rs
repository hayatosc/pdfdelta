//! Compare native and externally captured reading-order hypotheses.

use std::{
    env,
    io::{self, Write},
};

use pdfdelta_bench::revisions::order_probe;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report = order_probe::run_from_args(env::args().skip(1))?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer(&mut writer, &report)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
