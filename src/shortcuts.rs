//! Windows shortcuts: Desktop, Start Menu, and (optionally) Start with Windows.
//!
//! Every shortcut carries the app's taskbar identity (APP_ID, as its window does): a pin made
//! from one starts the app into that same taskbar button, not a second one beside it.

use std::path::{Path, PathBuf};

use anyhow::Result;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    IPersistFile, STGM_READWRITE,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Programs, FOLDERID_Startup, IShellLinkW, KF_FLAG_DEFAULT, SHCNE_ASSOCCHANGED,
    SHCNE_UPDATEITEM, SHCNF_IDLIST, SHCNF_PATHW, SHChangeNotify, SHGetKnownFolderPath, SHStrDupW, ShellLink,
};
use windows::core::{GUID, HSTRING, Interface};

use crate::config;

pub const NAME: &str = "Quiekel Embed.lnk";

/// COM for the calling thread, for as long as it's held.
struct Com(bool);

impl Com {
    fn new() -> Com {
        // SAFETY: undone in drop (unless COM was already set up differently on this thread).
        Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn known_folder(id: &GUID) -> Result<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)?;
        let path = PathBuf::from(p.to_string()?);
        CoTaskMemFree(Some(p.0 as *const _));
        Ok(path)
    }
}

/// Where pins on the taskbar keep their shortcuts.
fn taskbar_pins() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    appdata.join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar")
}

/// The shortcut at `path` starts this program (with `args`); one that's there already is updated
/// in place, keeping anything else set on it.
fn write(path: &Path, args: &str) -> Result<()> {
    let exe = std::env::current_exe()?;
    let workdir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let _com = Com::new();
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        if path.exists() {
            let _ = file.Load(&HSTRING::from(path), STGM_READWRITE);
        }
        link.SetPath(&HSTRING::from(exe.as_path()))?;
        link.SetArguments(&HSTRING::from(args))?;
        link.SetWorkingDirectory(&HSTRING::from(workdir.as_path()))?;
        link.SetIconLocation(&HSTRING::from(exe.as_path()), 0)?;
        link.SetDescription(&HSTRING::from("Search your files by meaning"))?;
        let mut id = PROPVARIANT::default();
        (*id.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*id.Anonymous.Anonymous).Anonymous.pwszVal = SHStrDupW(&HSTRING::from(config::APP_ID))?; // (freed with `id`)
        let store: IPropertyStore = link.cast()?;
        store.SetValue(&PKEY_AppUserModel_ID, &id)?;
        store.Commit()?;
        file.Save(&HSTRING::from(path), true)?;
        // Explorer shows the new target's icon at once.
        let shown = HSTRING::from(path);
        SHChangeNotify(SHCNE_UPDATEITEM, SHCNF_PATHW, Some(shown.as_ptr() as *const _), None);
    }
    Ok(())
}

fn startup_shortcut() -> Result<PathBuf> {
    Ok(known_folder(&FOLDERID_Startup)?.join(NAME))
}

pub fn autostart_enabled() -> bool {
    startup_shortcut().is_ok_and(|p| p.exists())
}

/// --hidden: start quietly in the tray and keep folders indexed.
pub fn set_autostart(enabled: bool) -> Result<()> {
    let path = startup_shortcut()?;
    if enabled {
        write(&path, "--hidden")
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// `--shortcuts [--remove]`: the Desktop and Start Menu shortcuts. Adding them also points a start
/// with Windows and a taskbar pin made before (by this version or an older one) at this program.
pub fn install(remove: bool) -> Result<()> {
    for folder in [&FOLDERID_Desktop, &FOLDERID_Programs] {
        let path = known_folder(folder)?.join(NAME);
        if remove {
            let _ = std::fs::remove_file(&path);
        } else {
            write(&path, "")?;
            println!("created {}", path.display());
        }
    }
    if remove {
        set_autostart(false)?;
        println!("removed Desktop, Start Menu and Startup shortcuts");
        return Ok(());
    }
    if autostart_enabled() {
        set_autostart(true)?;
    }
    let pinned = taskbar_pins().join(NAME);
    if pinned.exists() {
        write(&pinned, "")?;
    }
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) }; // (cached icons go)
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (target, arguments, taskbar identity, hotkey) of a shortcut.
    fn read(path: &Path) -> (String, String, String, u16) {
        let _com = Com::new();
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.cast::<IPersistFile>().unwrap().Load(&HSTRING::from(path), STGM_READWRITE).unwrap();
            let text = |buf: &[u16]| String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]);
            let (mut target, mut args) = ([0u16; 1024], [0u16; 1024]);
            link.GetPath(&mut target, std::ptr::null_mut(), 0).unwrap();
            link.GetArguments(&mut args).unwrap();
            let id = link.cast::<IPropertyStore>().unwrap().GetValue(&PKEY_AppUserModel_ID).unwrap();
            assert_eq!(id.Anonymous.Anonymous.vt, VT_LPWSTR);
            let id = id.Anonymous.Anonymous.Anonymous.pwszVal.to_string().unwrap();
            (text(&target), text(&args), id, link.GetHotkey().unwrap())
        }
    }

    #[test]
    fn a_shortcut_starts_this_program_and_carries_the_apps_taskbar_identity() {
        let dir = std::env::temp_dir().join(format!("quiekel-rs-lnk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lnk = dir.join(NAME);
        write(&lnk, "--hidden").unwrap();
        let exe = std::env::current_exe().unwrap().to_string_lossy().to_string();
        let (target, args, id, _) = read(&lnk);
        assert!(target.eq_ignore_ascii_case(&exe) && args == "--hidden" && id == config::APP_ID);
        // Something set on it meanwhile (here a hotkey) stays when it's written again.
        unsafe {
            let _com = Com::new();
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let file: IPersistFile = link.cast().unwrap();
            file.Load(&HSTRING::from(lnk.as_path()), STGM_READWRITE).unwrap();
            link.SetHotkey(0x0651).unwrap(); // (Ctrl+Shift+Q)
            file.Save(&HSTRING::from(lnk.as_path()), true).unwrap();
        }
        write(&lnk, "").unwrap();
        let (_, args, id, hotkey) = read(&lnk);
        assert_eq!((args.as_str(), id.as_str(), hotkey), ("", config::APP_ID, 0x0651));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
