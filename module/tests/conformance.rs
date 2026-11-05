use geko_module_sdk::module::{Context, DataPolicy};
use geko_module_sdk::scope::Scope;

#[test]
fn conformance() {
    let data = tempfile::tempdir().unwrap();
    let context = Context { config: String::new(), config_file: String::new(), scope: Scope::default(), data_dir: data.path().to_path_buf(), data: DataPolicy::default() };
    geko_module_sdk::conformance::module::check(&*geko_module_router::open(context).unwrap());
}
