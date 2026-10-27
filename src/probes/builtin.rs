//! Built-in promotional service probe: decode the bundled credential without
//! printing it and report whether this engine can use the service.

use anyhow::{bail, Result};

use crate::asr::private_trial;
use crate::asr::speech_models;

pub fn run(check: bool) -> Result<()> {
    let credential = private_trial::load()?;
    println!("payload: {}", private_trial::payload_path().display());
    println!("host: {}", credential.host);
    println!(
        "model: {}",
        speech_models::promotion_model().unwrap_or_default()
    );
    println!("private_trial: {}", crate::release_features::private_trial_enabled());
    if check {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        if runtime.block_on(private_trial::probe()).is_err() {
            bail!("内置服务连接失败，请检查网络或切换个人 API。");
        }
        println!("probe: ok");
    }
    Ok(())
}
