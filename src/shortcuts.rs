//! Windows shortcuts: Desktop, Start Menu, and (optionally) Start with Windows.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Result, bail};

pub const NAME: &str = "Quiekel Embed.lnk";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// Values arrive through environment variables, so paths with spaces need no quoting.
const PS: &str = r#"
$dir = [Environment]::GetFolderPath($env:QE_FOLDER)
$path = Join-Path $dir $env:QE_NAME
if ($env:QE_REMOVE -eq '1') { Remove-Item -LiteralPath $path -ErrorAction SilentlyContinue; exit 0 }
$s = (New-Object -ComObject WScript.Shell).CreateShortcut($path)
$s.TargetPath = $env:QE_TARGET
$s.Arguments = $env:QE_ARGS
$s.WorkingDirectory = $env:QE_WORKDIR
$s.IconLocation = "$($env:QE_TARGET),0"
$s.Description = 'Search your files by meaning'
$s.Save()
Write-Output $path
"#;

fn startup_dir() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    appdata.join("Microsoft").join("Windows").join("Start Menu").join("Programs").join("Startup")
}

fn shortcut(folder: &str, args: &str, remove: bool) -> Result<String> {
    let exe = std::env::current_exe()?;
    let workdir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", PS])
        .env("QE_FOLDER", folder)
        .env("QE_NAME", NAME)
        .env("QE_REMOVE", if remove { "1" } else { "0" })
        .env("QE_TARGET", &exe)
        .env("QE_ARGS", args)
        .env("QE_WORKDIR", &workdir)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn autostart_enabled() -> bool {
    startup_dir().join(NAME).exists()
}

/// --hidden: start quietly in the tray and keep folders indexed.
pub fn set_autostart(enabled: bool) -> Result<()> {
    shortcut("Startup", "--hidden", !enabled).map(|_| ())
}

/// `--shortcuts [--remove]`: the Desktop and Start Menu shortcuts.
pub fn install(remove: bool) -> Result<()> {
    for folder in ["Desktop", "Programs"] {
        let path = shortcut(folder, "", remove)?;
        if !path.is_empty() {
            println!("created {path}");
        }
    }
    if remove {
        set_autostart(false)?;
        println!("removed Desktop, Start Menu and Startup shortcuts");
    }
    Ok(())
}
