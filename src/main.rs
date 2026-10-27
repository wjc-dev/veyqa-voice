use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod asr;
mod audio;
mod config;
mod custom_vocabulary;
mod diagnostics;
mod engine;
mod engine_ipc;
mod health;
mod hotkey;
mod http;
mod insert;
mod instance_lock;
mod login_item;
mod paths;
mod permissions;
mod probes;
mod release_features;
mod runtime;
mod session;
mod settings;
mod settings_launch;
mod setup_feedback;
mod state;
mod text;
mod timeutil;
mod ui;
mod updater;

#[derive(Parser)]
#[command(name = "veyqa-voice-rs", version, about = "Veyqa Voice Rust engine / M0 probes")]
struct Cli {
    /// 把 stdout/stderr 追加写入文件（.app 经 open 启动时无法直接看到输出）
    #[arg(long, global = true)]
    log: Option<PathBuf>,
    /// Internal restart flag: reopen the settings window after relaunch.
    #[arg(long, global = true, hide = true)]
    show_settings_after_restart: bool,
    /// Internal restart flag: relaunch in the background.
    #[arg(long, global = true, hide = true)]
    background_login: bool,
    /// Internal flag: launched by the bundled supervisor at login.
    #[arg(long, global = true, hide = true)]
    supervised: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the dictation engine (menu bar by default, `--headless` for probes)
    Run {
        /// Skip AppKit and run the engine loop in the terminal.
        #[arg(long)]
        headless: bool,
    },
    /// 输出辅助功能/输入监控/发送事件授权状态
    Tcc {
        /// 触发系统授权请求（会弹出系统提示）
        #[arg(long)]
        request: bool,
    },
    /// 录音探针：设备选择、首帧延迟、回调抖动、RMS、16k WAV
    Audio {
        #[arg(long, default_value_t = 3.0)]
        seconds: f32,
        #[arg(long)]
        output: Option<PathBuf>,
        /// 按名称子串覆盖 AUDIO_INPUT_DEVICE
        #[arg(long)]
        device: Option<String>,
        /// 固定 CoreAudio 缓冲帧数（默认由设备决定）
        #[arg(long)]
        buffer: Option<u32>,
    },
    /// 只读 CGEventTap 探针：修饰键/组合键/Esc/修复循环
    Hotkey {
        #[arg(long, default_value_t = 10.0)]
        seconds: f32,
        /// 用合成事件自测（左 Option → K → 松开；不影响生产版右 Option 热键）
        #[arg(long)]
        self_test: bool,
    },
    /// AX 焦点锁定 + 全格式剪贴板快照/还原 + ⌘V 探针
    Paste {
        #[arg(long, default_value = "Veyqa Rust 粘贴探针：你好，世界。")]
        text: String,
        /// 等待用户切换焦点的秒数（粘贴默认 2s；只读检查默认 0）
        #[arg(long)]
        delay: Option<f32>,
        /// 只读取焦点 AX 元数据，不写剪贴板、不发 ⌘V
        #[arg(long)]
        inspect: bool,
    },
    /// 千问流式 ASR 探针：WAV → WebSocket → 中间/最终结果
    Asr {
        #[arg(long)]
        wav: PathBuf,
    },
    /// 内置服务探针：解码内测凭证（不打印密钥），可选真实连接
    Builtin {
        /// 真实发送一次静音校验（会消耗极少量服务额度）
        #[arg(long)]
        check: bool,
    },
}

fn redirect_output(path: Option<&PathBuf>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    unsafe {
        libc::dup2(fd, libc::STDOUT_FILENO);
        libc::dup2(fd, libc::STDERR_FILENO);
    }
    std::mem::forget(file);
    Ok(())
}

fn main() -> Result<()> {
    runtime::metrics::start_clock();
    let cli = Cli::parse();
    redirect_output(cli.log.as_ref())?;
    // Python's main.py runs ensure_runtime_layout() before load_dotenv, so a
    // first frozen launch reads the freshly copied .env in the same start.
    paths::ensure_runtime_layout()?;
    settings::sync_dev_hotkey_files();
    config::init()?;

    let result = match cli.command {
        Some(Command::Run { headless }) => {
            if headless {
                runtime::app::AppRuntime::run_headless()
            } else {
                ui::app::run()
            }
        }
        Some(Command::Tcc { request }) => probes::tcc::run(request),
        Some(Command::Audio {
            seconds,
            output,
            device,
            buffer,
        }) => probes::audio::run(seconds, output, device, buffer),
        Some(Command::Hotkey { seconds, self_test }) => probes::hotkey::run(seconds, self_test),
        Some(Command::Paste {
            text,
            delay,
            inspect,
        }) => probes::paste::run(text, delay.unwrap_or(if inspect { 0.0 } else { 2.0 }), inspect),
        Some(Command::Asr { wav }) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            runtime.block_on(asr::qwen::probe(&wav))
        }
        Some(Command::Builtin { check }) => probes::builtin::run(check),
        None => {
            if paths::is_frozen() {
                ui::app::run()
            } else {
                println!("veyqa-voice-rs — M0 probes");
                println!("  run [--headless]       启动听写引擎（默认菜单栏，--headless 无界面）");
                println!("  tcc                    权限状态");
                println!("  audio --seconds 3      录音探针");
                println!("  hotkey --seconds 10    只读快捷键探针（--self-test 自动按键）");
                println!("  paste --delay 3        焦点锁定 + 剪贴板还原 + ⌘V 探针");
                println!("  asr --wav <file>       千问流式 ASR 探针");
                println!("  builtin [--check]      内置服务凭证探针（--check 真实连接）");
                println!("使用 --help 查看参数。");
                Ok(())
            }
        }
    };
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    result
}
