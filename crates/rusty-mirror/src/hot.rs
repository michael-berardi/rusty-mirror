//! Serve staged frontend revisions from memory at `/__rusty_mirror/<rev>/…`.
//! Asset requests never touch the disk; `load` verifies a revision once.
use std::{
    borrow::Cow,
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};
use tauri::{
    utils::assets::{AssetKey, AssetsIter, CspHash},
    Assets, Context, Wry,
};

/// At most this many revisions stay resident per process.
const MAX_REVISIONS: usize = 8;

static REVISIONS: OnceLock<Mutex<BTreeMap<String, rusty_mirror_core::stage::Files>>> =
    OnceLock::new();
static INSTALLED: OnceLock<()> = OnceLock::new();

struct Placeholder;
impl Assets<Wry> for Placeholder {
    fn get(&self, _: &AssetKey) -> Option<Cow<'_, [u8]>> {
        None
    }
    fn iter(&self) -> Box<AssetsIter<'_>> {
        Box::new(std::iter::empty())
    }
    fn csp_hashes(&self, _: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
    }
}

struct Hot(Box<dyn Assets<Wry>>);
impl Assets<Wry> for Hot {
    fn setup(&self, app: &tauri::App<Wry>) {
        self.0.setup(app)
    }
    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        let prefix = rusty_mirror_core::HOT_PREFIX;
        if let Some(rest) = key.as_ref().strip_prefix(prefix) {
            let (revision, file) = rest.split_once('/')?;
            let file = if file.is_empty() { "index.html" } else { file };
            return REVISIONS
                .get()?
                .lock()
                .ok()?
                .get(revision)?
                .get(file)
                .cloned()
                .map(Cow::Owned);
        }
        self.0.get(key)
    }
    fn iter(&self) -> Box<AssetsIter<'_>> {
        self.0.iter()
    }
    fn csp_hashes(&self, key: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        if key.as_ref().starts_with(rusty_mirror_core::HOT_PREFIX) {
            Box::new(std::iter::empty())
        } else {
            self.0.csp_hashes(key)
        }
    }
}

/// Wrap the embedded assets so staged revisions can be served next to them.
/// Call once, before `run`. Without it, `refresh`/`apply` report unavailable.
pub fn install_hot_assets(context: &mut Context<Wry>) {
    let embedded = context.set_assets(Box::new(Placeholder));
    context.set_assets(Box::new(Hot(embedded)));
    let _ = INSTALLED.set(());
}

pub(crate) fn installed() -> bool {
    INSTALLED.get().is_some()
}

pub(crate) fn load(app_id: &str, revision: &str) -> Result<(), String> {
    if !installed() {
        return Err(
            "hot assets are not installed; call rusty_mirror::install_hot_assets(&mut context)"
                .into(),
        );
    }
    let revisions = REVISIONS.get_or_init(Default::default);
    if revisions
        .lock()
        .map_err(|_| "hot asset lock")?
        .contains_key(revision)
    {
        return Ok(());
    }
    let files = rusty_mirror_core::stage::load(app_id, revision)?;
    let mut map = revisions.lock().map_err(|_| "hot asset lock")?;
    if map.len() >= MAX_REVISIONS {
        return Err(format!(
            "{MAX_REVISIONS} revisions already loaded in this process; restart the debug build to load more"
        ));
    }
    map.insert(revision.to_owned(), files);
    Ok(())
}
