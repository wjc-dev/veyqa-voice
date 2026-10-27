use anyhow::Result;
use objc2_application_services::{AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFString, kCFBooleanTrue};
use objc2_core_graphics::{
    CGPreflightListenEventAccess, CGPreflightPostEventAccess, CGRequestListenEventAccess,
    CGRequestPostEventAccess,
};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

fn label(granted: bool) -> &'static str {
    if granted {
        "已授权"
    } else {
        "未授权"
    }
}

fn request_access() {
    let prompt_key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
    let prompt_value: &CFBoolean = unsafe { kCFBooleanTrue }.expect("kCFBooleanTrue");
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(
        &[prompt_key],
        &[prompt_value],
    );
    let options_ref: &CFDictionary = options.as_ref();
    let accessibility = unsafe { AXIsProcessTrustedWithOptions(Some(options_ref)) };
    let listen = CGRequestListenEventAccess();
    let post = CGRequestPostEventAccess();
    println!("已请求授权: accessibility={accessibility}, listen={listen}, post={post}");
    println!("在系统设置中授予后，重新运行 `tcc` 复核；输入监控授权通常需要重启应用。");
}

pub fn run(request: bool) -> Result<()> {
    if request {
        request_access();
        println!();
    }

    let accessibility = unsafe { AXIsProcessTrusted() != 0 };
    let input_monitoring = CGPreflightListenEventAccess();
    let post_events = CGPreflightPostEventAccess();
    let executable = std::env::current_exe()?;

    println!("probe: {}", executable.display());
    println!("辅助功能 Accessibility: {}", label(accessibility));
    println!("输入监控 Input Monitoring: {}", label(input_monitoring));
    println!("发送事件 Post Events: {}", label(post_events));

    if !accessibility || !input_monitoring || !post_events {
        println!();
        println!("授权路径：系统设置 → 隐私与安全性 → 辅助功能 / 输入监控");
        println!("从终端运行时，被授权的是终端或 Qoder，而不是当前二进制。");
        println!("快捷键与粘贴探针建议改用签名后的 .app 运行。");
    }
    Ok(())
}
