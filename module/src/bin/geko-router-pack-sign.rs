//! Signs the country packs of the router before they are published (`ci/packs.sh publish`), with the
//! same key as the other artifacts of geko.
//!
//! ```text
//! geko-router-pack-sign sign <pack folder>     writes signature.json with the key in GEKO_PACK_SIGNING_KEY (hex)
//! geko-router-pack-sign verify <pack folder>   checks it against the keys geko trusts
//! geko-router-pack-sign format                 the format of the packs this module reads: their tag
//! geko-router-pack-sign version <pack folder>  the version of the pack: its tag pack-<version>
//! ```

use std::path::Path;
use std::process::ExitCode;

use geko_artifact::signature;
use geko_module_router::pack::{CountryPack, FORMAT, KIND};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["sign", dir] => std::env::var("GEKO_PACK_SIGNING_KEY")
            .map_err(|_| "GEKO_PACK_SIGNING_KEY is not set".to_owned())
            .and_then(|key| signature::signing_key(&key).map_err(|e| e.to_string()))
            .and_then(|key| signature::sign(Path::new(dir), KIND, &key).map_err(|e| e.to_string())),
        ["verify", dir] => signature::verify_trusted(Path::new(dir), KIND).map_err(|e| e.to_string()),
        ["format"] => {
            println!("{FORMAT}");
            Ok(())
        }
        ["version", dir] => CountryPack::load(Path::new(dir)).map(|pack| println!("{}", pack.version)).map_err(|e| e.to_string()),
        _ => Err("usage: geko-router-pack-sign sign <pack folder> | verify <pack folder> | format | version <pack folder>".to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
