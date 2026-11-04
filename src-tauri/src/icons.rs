//! Icons as PNG data URLs, taken from the shell like Explorer and the Start menu do:
//! an app (`shell:AppsFolder\<AppUserModelID>`) or a file path → IShellItemImageFactory
//! → 32-bit bitmap → PNG.

use base64::Engine;
use windows::core::HSTRING;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};

pub fn app_icon_data_url(aumid: &str, size: i32) -> Option<String> {
    if aumid.is_empty() {
        return None;
    }
    shell_icon_data_url(&format!("shell:AppsFolder\\{aumid}"), size)
}

/// Icon of any shell item: a file path, or a shell: path.
pub fn shell_icon_data_url(parsing_name: &str, size: i32) -> Option<String> {
    unsafe {
        let path = HSTRING::from(parsing_name);
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&path, None).ok()?;
        let hbmp = factory.GetImage(SIZE { cx: size, cy: size }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;

        let mut bm = BITMAP::default();
        let got = GetObjectW(
            HGDIOBJ(hbmp.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut _),
        );
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let mut pixels = vec![0u8; (w.max(0) * h * 4) as usize];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let dc = CreateCompatibleDC(None);
        let lines = if got > 0 && w > 0 && h > 0 {
            GetDIBits(dc, hbmp, 0, h as u32, Some(pixels.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS)
        } else {
            0
        };
        let _ = DeleteDC(dc);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        if lines <= 0 {
            return None;
        }

        // BGRA, premultiplied alpha → straight RGBA
        let any_alpha = pixels.chunks(4).any(|p| p[3] != 0);
        for p in pixels.chunks_mut(4) {
            p.swap(0, 2);
            if !any_alpha {
                p[3] = 255; // old-style icon without an alpha channel
            } else if p[3] > 0 && p[3] < 255 {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }

        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, w as u32, h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().ok()?;
            writer.write_image_data(&pixels).ok()?;
        }
        Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png_bytes)))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore] // needs Teams/Outlook installed: run locally with --ignored
    fn icons_of_installed_apps() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        for aumid in [
            "MSTeams_8wekyb3d8bbwe!MSTeams",
            "Microsoft.OutlookForWindows_8wekyb3d8bbwe!Microsoft.OutlookforWindows",
            "Microsoft.Office.OUTLOOK.EXE.15",
            "Microsoft.Windows.Explorer",
        ] {
            let icon = super::app_icon_data_url(aumid, 64);
            println!("{aumid}: {}", icon.as_ref().map(|s| format!("{} bytes", s.len())).unwrap_or("NONE".into()));
            if let (Ok(dir), Some(url)) = (std::env::var("LUMEN_ICON_DUMP"), &icon) {
                use base64::Engine;
                let bytes = base64::engine::general_purpose::STANDARD.decode(url.trim_start_matches("data:image/png;base64,")).unwrap();
                let name: String = aumid.chars().filter(|c| c.is_ascii_alphanumeric()).take(20).collect();
                std::fs::write(std::path::Path::new(&dir).join(format!("{name}.png")), bytes).unwrap();
            }
        }
        assert!(super::app_icon_data_url("MSTeams_8wekyb3d8bbwe!MSTeams", 64).is_some());
    }
}
