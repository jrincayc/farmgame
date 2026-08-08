//! Copies `assets/` into `target/<profile>/assets/` on every build.
//!
//! Bevy's default `AssetPlugin` looks for files under a single root next
//! to the compiled binary (e.g. `target/debug/assets/`), with no
//! built-in fallback to the project root. The prototype spike confirmed
//! this trips people up. This is design-doc §5's "option (a)" — the
//! low-effort fix, sufficient until install-location behavior actually
//! matters for a release build (at which point option (b), a custom
//! `AssetSource`, would be worth the extra work).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src_assets = manifest_dir.join("assets");

    // OUT_DIR is something like target/<profile>/build/<crate>-<hash>/out.
    // Walk up to target/<profile>/.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target_profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR had fewer ancestors than expected")
        .to_path_buf();

    let dest_assets = target_profile_dir.join("assets");

    if src_assets.exists() {
        if let Err(e) = copy_dir_recursive(&src_assets, &dest_assets) {
            // Don't fail the build over this — worst case, Bevy will log
            // a clear "asset not found" at runtime, which is still more
            // actionable than a broken build.
            println!(
                "cargo:warning=failed to copy assets/ to {}: {e}",
                dest_assets.display()
            );
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
