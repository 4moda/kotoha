//! Local neural machine translation (`ct2rs`) adapter and composite 6-provider factory.
//!
//! Isolates optional C/C++ `CTranslate2` compilation behind the `native-ct2` Cargo feature flag so
//! default workspace builds and tests run rapidly without compiling native C++ dependencies.

pub mod backend;
pub mod config;
pub mod factory;
pub mod lang_tags;
pub mod translator;

#[cfg(feature = "native-ct2")]
pub use backend::native::Ct2NativeBackend;
pub use backend::{
    DefaultLocalNmtBackendLoader, LocalNmtBackend, LocalNmtBackendLoader, LocalTranslateParams,
};
pub use config::{
    compute_file_sha256_hex, detect_model_family, resolve_compute_device,
    resolve_compute_device_with_caps, resolve_model_profile, validate_model_dir_layout,
    validate_model_id, verify_file_sha256, HardwareCapabilities, LocalNmtConfig, LocalNmtFamily,
    ResolvedDevice,
};
pub use factory::{CompositeTranslatorFactory, LocalNmtTranslatorFactory};
pub use lang_tags::to_flores200;
pub use translator::LocalNmtTranslator;
