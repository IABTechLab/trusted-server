//! The rewrites `ts config migrate` makes, one for each change the stack
//! made to the settings document.
//!
//! Every message names a setting and never holds a value from the
//! document, because a document can hold secrets.

use toml_edit::{Array, DocumentMut, Item, Key, RawString, Table, Value, value};
use trusted_server_core::integrations::supplied_middleware;
use trusted_server_core::middleware::{HTML_MEDIA_TYPE, MiddlewarePhase};
use trusted_server_core::module_name::short_form;
use trusted_server_core::prefix::PublicPrefix;

use super::Adapter;
use super::layout::{Layout, Step, TablePath, path, prefix_of, set_prefix, to_crlf, uses_crlf};

/// The tables main read at the top level of a document, and `[permissions]`,
/// which main did not read and which an operator may add before migrating to
/// name the policy file this release requires. Main refused any other
/// table, so a document with `[publisher]` and only these tables was written
/// for main.
const MAIN_TABLES: &[&str] = &[
    "publisher",
    "permissions",
    "tester_cookie",
    "trusted_client_ip",
    "ec",
    "integrations",
    "handlers",
    "response_headers",
    "request_signing",
    "rewrite",
    "auction",
    "consent",
    "cache",
    "proxy",
    "creative_opportunities",
    "image_optimizer",
    "tinybird",
    "debug",
];

/// An `[integrations.<id>]` table main read, the value main gave `enabled`
/// when the table left it out, and whether the module that replaces it has
/// settings of its own.
struct MainIntegration {
    id: &'static str,
    enabled_by_default: bool,
    has_settings: bool,
}

/// Main's integrations that each became one module. `aps` and
/// `adserver_mock` became parts of the auction and are rewritten apart.
const MAIN_INTEGRATIONS: &[MainIntegration] = &[
    MainIntegration::new("prebid", true, true),
    MainIntegration::new("testlight", false, true),
    MainIntegration::new("nextjs", false, true),
    MainIntegration::new("permutive", true, true),
    MainIntegration::new("lockr", true, true),
    MainIntegration::new("didomi", true, true),
    MainIntegration::new("sourcepoint", false, true),
    MainIntegration::new("osano", false, false),
    MainIntegration::new("google_tag_manager", false, true),
    MainIntegration::new("datadome", false, true),
    MainIntegration::new("gpt", true, true),
    MainIntegration::new("gpt_diagnostics", false, false),
    MainIntegration::new("js_asset_proxy", false, true),
];

impl MainIntegration {
    const fn new(id: &'static str, enabled_by_default: bool, has_settings: bool) -> Self {
        Self {
            id,
            enabled_by_default,
            has_settings,
        }
    }
}

/// The item left to do in a document that names no permission policy file.
pub(crate) const POLICY_TO_DO: &str = "`[permissions] policy` names no permission policy \
     file, and a domain whose settings name none does not start. Name the publisher's file \
     there by its stem, as `policy = \"standard\"` names `standard.yaml`, and supply the \
     file with `ts config push --permissions <file>`";

/// The sections that select several modules, written with `modules` even
/// when one is selected.
const LIST_SECTIONS: &[&str] = &["tag", "ad-tag", "auction", "proxy", "ads-txt", "headers"];

/// Core's script proxy, which no linked builder describes.
const JS_ASSET_PROXY: &str = "js_asset_proxy";

/// The file paths of the middleware a `[[fetch]]` entry places on one file.
const FILE_MIDDLEWARE: &[(&str, &str)] =
    &[("ads-txt.iabtl", "/ads.txt"), ("robots_txt", "/robots.txt")];

/// The comment written above the first page entry the command adds.
const ENTRIES_COMMENT: &str = concat!(
    "\n# A module changes a page only where one of these entries names its\n",
    "# middleware, and a page takes the first entry that covers it.\n",
);

/// What a migration did and what it left.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// A line for each rewrite made.
    pub(crate) changed: Vec<String>,
    /// A line for each item the document cannot load without, or setting the
    /// stack ignores whose value the operator carries elsewhere, which needs
    /// a decision or a value the document does not hold.
    pub(crate) to_do: Vec<String>,
    /// A line for each change in behavior no setting expresses.
    pub(crate) check: Vec<String>,
}

/// A migrated document and its report.
#[derive(Debug)]
pub(crate) struct Migration {
    /// The document in the shape the stack reads, the input itself when
    /// nothing changed.
    pub(crate) text: String,
    /// What the migration did and what it left.
    pub(crate) report: Report,
}

/// Rewrites `input`, a settings document, into the shape the stack reads on
/// `adapter`.
///
/// # Errors
///
/// When `input` is not TOML, naming the line reading stopped at and nothing
/// from the line.
pub(crate) fn migrate(input: &str, adapter: Adapter) -> Result<Migration, String> {
    let doc = input
        .parse::<DocumentMut>()
        .map_err(|error| match error.span() {
            Some(span) => format!(
                "the document is not TOML, from line {}",
                input
                    .get(..span.start)
                    .map_or(1, |head| head.matches('\n').count() + 1)
            ),
            None => "the document is not TOML".to_owned(),
        })?;
    let mut migrator = Migrator::new(doc, adapter);
    migrator.run();
    let Migrator { layout, report, .. } = migrator;
    let text = if report.changed.is_empty() {
        input.to_owned()
    } else if uses_crlf(input) {
        to_crlf(&layout.finish())
    } else {
        layout.finish()
    };
    Ok(Migration { text, report })
}

/// A module a section can select, as the builders the tool links describe
/// it.
struct KnownModule {
    id: &'static str,
    section: &'static str,
    written: &'static str,
    full: &'static str,
}

/// A `[[fetch]]` or `[[serve]]` entry the migration adds.
struct PageEntry {
    phase: &'static str,
    media_type: &'static str,
    path: Option<&'static str>,
    middleware: Vec<&'static str>,
}

/// The document being migrated, the adapter it is for, what the migration
/// has reported, and what the tool's builders say of the modules.
struct Migrator {
    layout: Layout,
    adapter: Adapter,
    report: Report,
    modules: Vec<KnownModule>,
    shared: Vec<(&'static str, &'static str)>,
    for_main: bool,
}

impl Migrator {
    fn new(doc: DocumentMut, adapter: Adapter) -> Self {
        let builders = crate::app_config::linked_builders();
        let mut modules = vec![KnownModule {
            id: JS_ASSET_PROXY,
            section: "proxy",
            written: JS_ASSET_PROXY,
            full: JS_ASSET_PROXY,
        }];
        let mut shared = Vec::new();
        for builder in &builders {
            if let (Some(section), Some(full)) = (builder.section(), builder.module_name()) {
                modules.push(KnownModule {
                    id: builder.id(),
                    section,
                    written: short_form(section, full),
                    full,
                });
            }
            if let Some(vendor) = builder.shared_settings() {
                shared.push((builder.id(), vendor));
            }
        }
        let for_main = written_for_main(doc.as_table());
        Self {
            layout: Layout::new(doc),
            adapter,
            report: Report::default(),
            modules,
            shared,
            for_main,
        }
    }

    fn run(&mut self) {
        self.publisher_source();
        self.asset_routes();
        self.handlers();
        self.request_signing();
        self.edge_cookie();
        self.permission_signal();
        self.device();
        self.demand();
        self.integrations();
        self.mediator_left();
        self.ad_server_names();
        self.ads_txt_names();
        self.tinybird();
        self.debug_options();
        self.consent_lists();
        self.permission_policy();
        self.template_cookies();
        if self.for_main {
            self.main_defaults();
        }
        self.location_at_startup();
        self.page_entries();
        if self.for_main {
            self.main_checks();
        }
    }

    fn root(&self) -> &Table {
        self.layout.doc.as_table()
    }

    fn root_mut(&mut self) -> &mut Table {
        self.layout.doc.as_table_mut()
    }

    fn push_changed(&mut self, line: impl Into<String>) {
        self.report.changed.push(line.into());
    }

    fn push_to_do(&mut self, line: impl Into<String>) {
        self.report.to_do.push(line.into());
    }

    fn push_check(&mut self, line: impl Into<String>) {
        self.report.check.push(line.into());
    }

    /// `[publisher]`'s origin settings into `[source]`, which selects core's
    /// `origin` source and holds them in `[source.origin]`.
    fn publisher_source(&mut self) {
        const MOVES: &[(&str, &str)] = &[
            ("origin_url", "url"),
            ("origin_host_header_override", "host_header"),
            ("directory_index", "directory_index"),
            ("origin_auth", "auth"),
        ];
        let Some(publisher) = self.root().get("publisher").and_then(Item::as_table) else {
            return;
        };
        let moving = keys_in_order(publisher, MOVES.iter().map(|(old, _)| *old));
        if moving.is_empty() {
            return;
        }
        if self.root().contains_key("source") {
            self.push_to_do(
                "`[publisher]` holds origin settings and `[source]` is written too. Keep one of \
                 the two, with the origin's settings in `[source.origin]`",
            );
            return;
        }
        let anchor = self.layout.position_of(&path(&["publisher"]));
        let Some(publisher) = self.layout.table_mut(&path(&["publisher"])) else {
            return;
        };
        let mut origin = Table::new();
        let mut lines = Vec::new();
        for old in &moving {
            let Some(new) = MOVES
                .iter()
                .find(|(from, _)| from == old)
                .map(|(_, to)| *to)
            else {
                continue;
            };
            if let Some((key, item)) = publisher.remove_entry(old) {
                origin.insert_formatted(&renamed(&key, new), item);
                lines.push(if lines.is_empty() {
                    format!(
                        "moved `[publisher] {old}` to `[source.origin] {new}`, with `[source] \
                         module = \"origin\"`"
                    )
                } else {
                    format!("moved `[publisher] {old}` to `[source.origin] {new}`")
                });
            }
        }
        for line in lines {
            self.push_changed(line);
        }
        self.rename_signer(&mut origin, "`[source.origin.auth] type`");
        if origin
            .get("url")
            .and_then(Item::as_str)
            .and_then(|url| url::Url::parse(url).ok())
            .is_some_and(|url| !url.path().is_empty() && url.path() != "/")
        {
            self.push_check(
                "`[source.origin] url` has a path, which main dropped and this release keeps, so \
                 each request now goes to the folder the path names. Check the origin serves the \
                 site there, or remove the path",
            );
        }
        if origin.contains_key("host_header") {
            self.push_check(
                "`[source.origin] host_header` is set, so the addresses a page gives under that \
                 host are now rewritten to the request's host, where main rewrote those under the \
                 host of `[source.origin] url`. Check which host the origin's pages name",
            );
        }
        let auth_is_table = origin.get("auth").is_some_and(Item::is_table);
        let mut source = Table::new();
        source.insert("module", value("origin"));
        source.insert("origin", Item::Table(origin));
        self.root_mut().insert("source", Item::Table(source));
        let mut new = vec![path(&["source"]), path(&["source", "origin"])];
        if auth_is_table {
            new.push(path(&["source", "origin", "auth"]));
        }
        let anchor = anchor.unwrap_or_else(|| self.layout.last_position());
        self.layout.place_after(anchor, &new);
    }

    /// Each `[[proxy.asset_routes]]` entry's `origin_url` and `auth` into its
    /// `source` table.
    fn asset_routes(&mut self) {
        const MOVES: &[(&str, &str)] = &[("origin_url", "url"), ("auth", "auth")];
        let Some(routes) = self
            .root()
            .get("proxy")
            .and_then(|proxy| proxy.get("asset_routes"))
            .and_then(Item::as_array_of_tables)
        else {
            return;
        };
        let count = routes.len();
        for index in 0..count {
            let number = index + 1;
            let route_path = vec![
                Step::Key("proxy".to_owned()),
                Step::Key("asset_routes".to_owned()),
                Step::Index(index),
            ];
            let anchor = self.layout.position_of(&route_path);
            let Some(route) = self.layout.table_mut(&route_path) else {
                continue;
            };
            let moving = keys_in_order(route, MOVES.iter().map(|(old, _)| *old));
            if moving.is_empty() {
                continue;
            }
            if route.contains_key("source") {
                self.push_to_do(format!(
                    "`[[proxy.asset_routes]]` entry {number} holds origin settings and a \
                     `source` table too. Keep one of the two"
                ));
                continue;
            }
            let mut origin = Table::new();
            for old in &moving {
                if let Some((key, item)) = route.remove_entry(old) {
                    let new = MOVES
                        .iter()
                        .find(|(from, _)| from == old)
                        .map_or(*old, |(_, to)| *to);
                    origin.insert_formatted(&renamed(&key, new), item);
                }
            }
            for old in &moving {
                let new = MOVES
                    .iter()
                    .find(|(from, _)| from == old)
                    .map_or(*old, |(_, to)| *to);
                self.push_changed(format!(
                    "moved `{old}` of `[[proxy.asset_routes]]` entry {number} to \
                     `[proxy.asset_routes.source.origin] {new}`, with `module = \"origin\"`"
                ));
            }
            self.rename_signer(
                &mut origin,
                &format!("The `auth` type of `[[proxy.asset_routes]]` entry {number}"),
            );
            let auth_is_table = origin.get("auth").is_some_and(Item::is_table);
            let mut source = Table::new();
            source.insert("module", value("origin"));
            source.insert("origin", Item::Table(origin));
            let Some(route) = self.layout.table_mut(&route_path) else {
                continue;
            };
            route.insert("source", Item::Table(source));
            let mut source_path = route_path.clone();
            source_path.push(Step::Key("source".to_owned()));
            let mut origin_path = source_path.clone();
            origin_path.push(Step::Key("origin".to_owned()));
            let mut new = vec![source_path, origin_path.clone()];
            if auth_is_table {
                let mut auth_path = origin_path;
                auth_path.push(Step::Key("auth".to_owned()));
                new.push(auth_path);
            }
            let anchor = anchor.unwrap_or_else(|| self.layout.last_position());
            self.layout.place_after(anchor, &new);
        }
    }

    /// The spelling `s3_sig_v4` of the S3 signer, which the stack no longer
    /// reads, in the `auth` table of `origin`.
    fn rename_signer(&mut self, origin: &mut Table, setting: &str) {
        let Some(auth) = origin.get_mut("auth").and_then(Item::as_table_like_mut) else {
            return;
        };
        let Some(signer) = auth.get_mut("type") else {
            return;
        };
        if signer.as_str() == Some("s3_sig_v4") {
            set_string(signer, "s3_sigv4");
            self.push_changed(format!(
                "{setting} uses the old spelling of the S3 signer, now `s3_sigv4`"
            ));
        }
    }

    /// `[[handlers]]`, removed where an entry guards only paths this release
    /// closes, and left for the operator otherwise.
    fn handlers(&mut self) {
        let Some(item) = self.root().get("handlers") else {
            return;
        };
        let Some(entries) = item.as_array_of_tables() else {
            self.push_to_do(
                "`handlers` is not read, and no request is asked for a password. Put a password \
                 on the paths it guards at the origin or in the hosting platform, then remove it",
            );
            return;
        };
        let patterns: Vec<Option<String>> = entries
            .iter()
            .map(|entry| entry.get("path").and_then(Item::as_str).map(str::to_owned))
            .collect();
        let mut changed = Vec::new();
        let mut to_do = Vec::new();
        for (index, pattern) in patterns.iter().enumerate().rev() {
            let number = index + 1;
            if pattern.as_deref().is_some_and(guards_only_closed_paths) {
                self.layout
                    .remove(&[Step::Key("handlers".to_owned()), Step::Index(index)]);
                changed.push(format!(
                    "removed `[[handlers]]` entry {number}, which guarded only paths this release \
                     closes, where nothing is served now"
                ));
            } else {
                to_do.push(format!(
                    "`[[handlers]]` entry {number} can guard paths beyond the `/_ts/admin` and \
                     `/admin/keys` paths this release closes, and no request is asked for a \
                     password now. Put a password on those paths at the origin or in the hosting \
                     platform, then remove the entry"
                ));
            }
        }
        self.report.changed.extend(changed.into_iter().rev());
        self.report.to_do.extend(to_do.into_iter().rev());
    }

    /// `[request_signing]`'s store ids, which the service does not read.
    fn request_signing(&mut self) {
        let Some(signing) = self
            .root_mut()
            .get_mut("request_signing")
            .and_then(Item::as_table_like_mut)
        else {
            return;
        };
        let removed: Vec<&str> = ["config_store_id", "secret_store_id"]
            .into_iter()
            .filter(|key| signing.remove(key).is_some())
            .collect();
        if removed.is_empty() {
            return;
        }
        for key in removed {
            self.push_changed(format!(
                "removed `[request_signing] {key}`, which the service does not read"
            ));
        }
        self.push_check(
            "give the store ids `[request_signing]` held to `ts keys rotate` and `ts keys \
             deactivate`, as `--config-store-id` and `--secret-store-id`",
        );
    }

    /// The Edge Cookie section: the deprecated `[ec] passphrase` into the
    /// `hmac` module's table, and the old table and module names.
    fn edge_cookie(&mut self) {
        const RENAMES: &[(&str, &str)] = &[
            ("host-signals", "host_signals"),
            ("client-fixed", "client_fixed"),
        ];
        let ec_path = path(&["ec"]);
        let anchor = self.layout.position_of(&ec_path);
        let Some(ec) = self.layout.table_mut(&ec_path) else {
            return;
        };
        let mut changed = Vec::new();
        let mut to_do = Vec::new();

        let renamed_block = |name: &str| {
            RENAMES
                .iter()
                .find(|(old, _)| *old == name)
                .map_or(name.to_owned(), |(_, new)| (*new).to_owned())
        };
        if let Some(providers) = ec.get("providers").and_then(Item::as_table) {
            let names = keys_of(providers);
            if let Some(taken) = names
                .iter()
                .find(|name| ec.contains_key(&renamed_block(name)))
            {
                to_do.push(format!(
                    "`[ec.providers.{taken}]` and `[ec.{}]` are both written. Keep one",
                    renamed_block(taken)
                ));
            } else if let Some(Ok(mut providers)) = ec.remove("providers").map(Item::into_table) {
                for name in names {
                    let Some((key, item)) = providers.remove_entry(&name) else {
                        continue;
                    };
                    let new = renamed_block(&name);
                    ec.insert_formatted(&renamed(&key, &new), item);
                    changed.push(format!("moved `[ec.providers.{name}]` to `[ec.{new}]`"));
                }
            }
        }
        for (old, new) in RENAMES {
            if ec.get(old).is_some_and(Item::is_table) && !ec.contains_key(new) {
                rename_key(ec, old, new);
                changed.push(format!("renamed `[ec.{old}]` to `[ec.{new}]`"));
            }
        }
        if let Some(module) = ec.get_mut("module")
            && let Some((_, new)) = RENAMES
                .iter()
                .find(|(old, _)| module.as_str() == Some(*old))
        {
            set_string(module, new);
            changed.push(format!(
                "renamed the module `[ec] module` selects to `{new}`"
            ));
        }
        for name in keys_of(ec) {
            let Some(block) = ec.get_mut(&name).and_then(Item::as_table_like_mut) else {
                continue;
            };
            let Some(implementation) = block.get_mut("implementation") else {
                continue;
            };
            if let Some((_, new)) = RENAMES
                .iter()
                .find(|(old, _)| implementation.as_str() == Some(*old))
            {
                set_string(implementation, new);
                changed.push(format!(
                    "renamed the module `[ec.{name}] implementation` names to `{new}`"
                ));
            }
        }

        let mut moved_passphrase = false;
        if ec.contains_key("passphrase") {
            let selects = ec.contains_key("module")
                || ec
                    .iter()
                    .any(|(_, item)| item.is_table() || item.is_inline_table());
            if selects {
                to_do.push(
                    "`[ec] passphrase` sits beside an Edge Cookie module of the new form. Keep \
                     one form, with the passphrase in the selected module's table, as \
                     `[ec.hmac] passphrase`"
                        .to_owned(),
                );
            } else if let Some((key, item)) = ec.remove_entry("passphrase") {
                let mut hmac = Table::new();
                hmac.insert_formatted(&key, item);
                ec.insert("hmac", Item::Table(hmac));
                insert_first(ec, "module", value("hmac"));
                changed.push(
                    "moved `[ec] passphrase` to `[ec.hmac] passphrase`, with `[ec] module = \
                     \"hmac\"`"
                        .to_owned(),
                );
                moved_passphrase = true;
            }
        }
        self.report.changed.extend(changed);
        self.report.to_do.extend(to_do);
        if moved_passphrase {
            let anchor = anchor.unwrap_or_else(|| self.layout.last_position());
            self.layout.place_after(anchor, &[path(&["ec", "hmac"])]);
        }
    }

    /// `[permission_signal] sources` into `[permission-signal] modules`, with
    /// the module names the stack renamed.
    fn permission_signal(&mut self) {
        const RENAMES: &[(&str, &str)] = &[
            ("gpp-sale-opt-out", "gpp"),
            ("gpp_sale_opt_out", "gpp"),
            ("us_privacy", "us-privacy"),
        ];
        if self.root().contains_key("permission_signal") {
            if self.root().contains_key("permission-signal") {
                self.push_to_do(
                    "`[permission_signal]` and `[permission-signal]` are both written. Keep \
                     `[permission-signal]`",
                );
                return;
            }
            rename_key(self.root_mut(), "permission_signal", "permission-signal");
            self.push_changed("renamed `[permission_signal]` to `[permission-signal]`");
        }
        let Some(section) = self
            .root_mut()
            .get_mut("permission-signal")
            .and_then(Item::as_table_mut)
        else {
            return;
        };
        let mut changed = Vec::new();
        if section.contains_key("sources") && !section.contains_key("modules") {
            rename_key(section, "sources", "modules");
            changed.push("renamed `[permission-signal] sources` to `modules`".to_owned());
        }
        if let Some(modules) = section.get_mut("modules").and_then(Item::as_array_mut) {
            for (old, new) in RENAMES {
                let Some(index) = modules.iter().position(|name| name.as_str() == Some(*old))
                else {
                    continue;
                };
                if modules.iter().any(|name| name.as_str() == Some(*new)) {
                    modules.remove(index);
                } else if let Some(name) = modules.get_mut(index) {
                    set_value_string(name, new);
                }
                changed.push(format!(
                    "renamed a module `[permission-signal] modules` names to `{new}`"
                ));
            }
        }
        self.report.changed.extend(changed);
    }

    /// `[device] module = "fastly"`, which the stack spells `platform`.
    fn device(&mut self) {
        let Some(module) = self
            .root_mut()
            .get_mut("device")
            .and_then(Item::as_table_like_mut)
            .and_then(|device| device.get_mut("module"))
        else {
            return;
        };
        if module.as_str() == Some("fastly") {
            set_string(module, "platform");
            self.push_changed(
                "`[device] module` now selects `platform` for the platform's own device module",
            );
        }
    }

    /// `[auction.providers.<id>]` into `[demand.<name>]`, selected in
    /// `[demand] modules`, and each bidder route's `provider` into `module`.
    fn demand(&mut self) {
        let providers_path = path(&["auction", "providers"]);
        let Some(providers) = self.layout.table_mut(&providers_path) else {
            return;
        };
        let ids: Vec<String> = providers
            .iter()
            .filter(|(_, item)| item.is_table())
            .map(|(key, _)| key.to_owned())
            .collect();
        if ids.is_empty() {
            return;
        }
        if self.root().contains_key("demand") {
            self.push_to_do(
                "`[auction.providers]` and `[demand]` are both written. Move each provider to \
                 `[demand.<name>]`, selected in `[demand] modules`",
            );
            return;
        }
        let mut names: Vec<(String, String)> = Vec::new();
        let mut tables = Vec::new();
        for id in &ids {
            let mut provider_path = providers_path.clone();
            provider_path.push(Step::Key(id.clone()));
            let profile = self.layout.table_mut(&provider_path).and_then(|table| {
                table
                    .get("profile")
                    .map(|item| item.as_str().map(str::to_owned))
            });
            let implementation = match profile {
                None => Some("auction-protocol.openrtb"),
                Some(Some(profile)) => implementation_of(&profile),
                Some(None) => None,
            };
            let Some(implementation) = implementation else {
                self.push_to_do(format!(
                    "`[auction.providers.{id}] profile` names no implementation this release \
                     has, so the provider stays where it is. Move it to `[demand.<name>]` with \
                     an `implementation` line by hand"
                ));
                continue;
            };
            let collides = self.layout.table_mut(&provider_path).is_some_and(|table| {
                table
                    .get("profile_config")
                    .and_then(Item::as_table_like)
                    .is_some_and(|config| {
                        config
                            .iter()
                            .any(|(key, _)| key == "implementation" || table.contains_key(key))
                    })
            });
            if collides {
                self.push_to_do(format!(
                    "`[auction.providers.{id}.profile_config]` holds a setting named as one of \
                     the provider's own, so the provider stays where it is. Move it to \
                     `[demand.<name>]` by hand"
                ));
                continue;
            }
            let Some(mut table) = self.layout.take(&provider_path) else {
                continue;
            };
            table.remove("protocol");
            let profile_key = table.remove_entry("profile").map(|(key, _)| key);
            if let Some(config) = table.remove("profile_config") {
                flatten_into(&mut table, config);
            }
            let key = profile_key.map_or_else(
                || Key::new("implementation"),
                |key| renamed(&key, "implementation"),
            );
            insert_first_formatted(&mut table, &key, value(implementation));
            let name = id.replace('-', "_");
            self.push_changed(format!(
                "moved `[auction.providers.{id}]` to `[demand.{name}]`, with `implementation = \
                 \"{implementation}\"` and its `profile_config` settings in the table"
            ));
            names.push((id.clone(), name.clone()));
            tables.push((name, table));
        }
        if self
            .layout
            .table_mut(&providers_path)
            .is_some_and(|providers| providers.is_empty())
        {
            self.layout.remove(&providers_path);
        }
        if tables.is_empty() {
            return;
        }

        let mut demand = Table::new();
        let selected: Array = tables.iter().map(|(name, _)| name.as_str()).collect();
        demand.insert("modules", value(selected));
        for (index, (name, mut table)) in tables.into_iter().enumerate() {
            if index == 0 {
                if let Some(position) = table.position() {
                    demand.set_position(position);
                }
                set_prefix(&mut demand, &prefix_of(&table));
                set_prefix(&mut table, "\n");
            }
            demand.insert(&name, Item::Table(table));
        }
        self.root_mut().insert("demand", Item::Table(demand));

        let Some(bidders) = self
            .root_mut()
            .get_mut("auction")
            .and_then(|auction| auction.get_mut("bidders"))
            .and_then(Item::as_table_mut)
        else {
            return;
        };
        let mut changed = Vec::new();
        for code in keys_of(bidders) {
            let Some(route) = bidders.get_mut(&code).and_then(Item::as_table_mut) else {
                continue;
            };
            if !route.contains_key("provider") || route.contains_key("module") {
                continue;
            }
            rename_key(route, "provider", "module");
            if let Some(module) = route.get_mut("module")
                && let Some((_, name)) = names
                    .iter()
                    .find(|(id, _)| module.as_str() == Some(id.as_str()))
            {
                set_string(module, name);
            }
            changed.push(format!(
                "renamed `[auction.bidders.{code}] provider` to `module`, naming the demand \
                 source by its new name"
            ));
        }
        self.report.changed.extend(changed);
    }

    /// Every `[integrations.<id>]` table, each into the module that replaced
    /// it, and `[integration]`, which is left for the operator.
    fn integrations(&mut self) {
        if self.root().contains_key("integration") {
            self.push_to_do(
                "`[integration]` is not read. Select each module in the section of its type, \
                 with its settings in `[<section>.<name>]`",
            );
        }
        let Some(integrations) = self.root().get("integrations") else {
            return;
        };
        let Some(integrations) = integrations.as_table() else {
            self.push_to_do(
                "`integrations` is not read. Select each module in the section of its type, \
                 with its settings in `[<section>.<name>]`",
            );
            return;
        };
        let ids = keys_of(integrations);
        for id in ids {
            self.layout
                .make_table(&path(&["integrations", id.as_str()]));
            match id.as_str() {
                "aps" => self.aps(),
                "adserver_mock" => self.adserver_mock(),
                _ => match MAIN_INTEGRATIONS.iter().find(|entry| entry.id == id) {
                    Some(entry) => self.integration(entry),
                    None => self.push_to_do(format!(
                        "`[integrations.{id}]` names no module this release has. Select the \
                         module that does its job in the section of its type, or remove the \
                         table"
                    )),
                },
            }
        }
        if self
            .root()
            .get("integrations")
            .and_then(Item::as_table)
            .is_some_and(Table::is_empty)
        {
            self.layout.remove(&path(&["integrations"]));
        }
    }

    /// One `[integrations.<id>]` table into its module.
    fn integration(&mut self, entry: &MainIntegration) {
        let id = entry.id;
        let table_path = path(&["integrations", id]);
        let Some((section, written)) = self.target(id) else {
            self.push_to_do(format!(
                "`[integrations.{id}]` names a module this build of the tool does not link"
            ));
            return;
        };
        let Some(table) = self.layout.table_mut(&table_path) else {
            return;
        };
        let enabled = match table.get("enabled") {
            None => entry.enabled_by_default,
            Some(item) => {
                if let Some(enabled) = item.as_bool() {
                    enabled
                } else {
                    self.push_to_do(format!(
                        "`[integrations.{id}] enabled` is not true or false, so it cannot say \
                         whether the module runs"
                    ));
                    return;
                }
            }
        };
        table.remove("enabled");
        let empty = table.is_empty();
        let mut header = vec![section];
        header.extend(written.split('.'));

        if !enabled {
            if entry.has_settings {
                self.layout.hold(&table_path, Some(&header[..]));
                self.push_changed(format!(
                    "commented out `[integrations.{id}]` as `[{}]`, because it was not enabled",
                    header.join(".")
                ));
            } else {
                self.layout.hold(&table_path, None);
                self.push_changed(format!(
                    "removed `[integrations.{id}]`, which was not enabled and has no settings, \
                     keeping the comments above it"
                ));
            }
            return;
        }

        if !entry.has_settings || empty {
            let position = self.layout.position_of(&table_path);
            if self.root().contains_key(section) {
                self.layout.remove(&table_path);
            } else if let Some(table) = self.layout.take(&table_path) {
                self.ensure_section(section, position, &prefix_of(&table));
            }
            let key = self.select(section, written);
            self.push_changed(format!(
                "selected `{written}` in `[{section}] {key}` for `[integrations.{id}]`, which \
                 sets nothing else"
            ));
            return;
        }

        let Some(mut table) = self.layout.take(&table_path) else {
            return;
        };
        if self.ensure_section(section, table.position(), &prefix_of(&table)) {
            set_prefix(&mut table, "\n");
        }
        insert_at(self.root_mut(), &header, table);
        let key = self.select(section, written);
        self.push_changed(format!(
            "moved `[integrations.{id}]` to `[{}]`, selected in `[{section}] {key}`",
            header.join(".")
        ));
    }

    /// `[integrations.aps] rendering_mode` into each `auction.aps` demand
    /// source. Main read it only from an enabled table.
    fn aps(&mut self) {
        let table_path = path(&["integrations", "aps"]);
        let Some(table) = self.layout.table_mut(&table_path) else {
            return;
        };
        let enabled = match table.get("enabled") {
            None => false,
            Some(item) => {
                if let Some(enabled) = item.as_bool() {
                    enabled
                } else {
                    self.push_to_do(
                        "`[integrations.aps] enabled` is not true or false, so it cannot say \
                         whether `rendering_mode` applies",
                    );
                    return;
                }
            }
        };
        let mode = table.get("rendering_mode").cloned();
        let sources: Vec<String> = self
            .root()
            .get("demand")
            .and_then(Item::as_table)
            .map(|demand| {
                demand
                    .iter()
                    .filter(|(_, item)| {
                        item.get("implementation").and_then(Item::as_str) == Some("auction.aps")
                    })
                    .map(|(name, _)| name.to_owned())
                    .collect()
            })
            .unwrap_or_default();
        match mode {
            Some(mode) if enabled => {
                if sources.is_empty() {
                    self.push_to_do(
                        "`[integrations.aps] rendering_mode` belongs in the table of an \
                         `auction.aps` demand source, and the document has none. Add it to that \
                         source's `[demand.<name>]` table, then remove `[integrations.aps]`",
                    );
                    return;
                }
                for name in &sources {
                    if let Some(source) = self
                        .root_mut()
                        .get_mut("demand")
                        .and_then(|demand| demand.get_mut(name))
                        .and_then(Item::as_table_mut)
                        && !source.contains_key("rendering_mode")
                    {
                        source.insert("rendering_mode", mode.clone());
                    }
                }
                self.layout.remove(&table_path);
                self.push_changed(
                    "moved `[integrations.aps] rendering_mode` into the table of each \
                     `auction.aps` demand source",
                );
            }
            mode => {
                self.layout.remove(&table_path);
                self.push_changed(
                    "removed `[integrations.aps]`, which sets nothing this release reads",
                );
                if mode.is_some() {
                    self.push_check(
                        "main did not apply `[integrations.aps] rendering_mode`, because the \
                         table was not enabled. Set `rendering_mode` in an `auction.aps` demand \
                         source's table to use it",
                    );
                }
            }
        }
    }

    /// `[integrations.adserver_mock]` into `[ad-server.iabtl]`, selected in
    /// place of `[auction] mediator`, which named it.
    fn adserver_mock(&mut self) {
        let table_path = path(&["integrations", "adserver_mock"]);
        let mediator = self
            .root()
            .get("auction")
            .and_then(|auction| auction.get("mediator"))
            .and_then(Item::as_str)
            .map(str::to_owned);
        let names_it = mediator.as_deref() == Some("adserver_mock");
        if names_it
            && (self.root().contains_key("ad-server") || self.root().contains_key("adserver"))
        {
            self.push_to_do(
                "`[auction] mediator` and `[ad-server]` are both written. Keep `[ad-server]`",
            );
            return;
        }
        let Some(table) = self.layout.table_mut(&table_path) else {
            return;
        };
        table.remove("enabled");
        let header = ["ad-server", "iabtl"];
        if !names_it {
            self.layout.hold(&table_path, Some(&header[..]));
            self.push_changed(
                "commented out `[integrations.adserver_mock]` as `[ad-server.iabtl]`, because \
                 `[auction] mediator` does not name it",
            );
            return;
        }
        let Some(mut table) = self.layout.take(&table_path) else {
            return;
        };
        if self.ensure_section("ad-server", table.position(), &prefix_of(&table)) {
            set_prefix(&mut table, "\n");
        }
        insert_at(self.root_mut(), &header, table);
        self.select("ad-server", "iabtl");
        if let Some(auction) = self
            .root_mut()
            .get_mut("auction")
            .and_then(Item::as_table_like_mut)
        {
            auction.remove("mediator");
        }
        self.push_changed(
            "moved `[integrations.adserver_mock]` to `[ad-server.iabtl]`, selected with \
             `[ad-server] module = \"iabtl\"` in place of `[auction] mediator`",
        );
    }

    /// An `[auction] mediator` the mock ad server's table did not come with.
    /// A mediator left beside that table is reported where the table is.
    fn mediator_left(&mut self) {
        let mock_table = self
            .root()
            .get("integrations")
            .is_some_and(|integrations| integrations.get("adserver_mock").is_some());
        if !mock_table
            && self
                .root()
                .get("auction")
                .is_some_and(|auction| auction.get("mediator").is_some())
        {
            self.push_to_do(
                "`[auction] mediator` names an ad server whose settings the document does not \
                 hold. Select the ad server with `[ad-server] module = \"iabtl\"`, give it an \
                 `[ad-server.iabtl]` table, then remove the line",
            );
        }
    }

    /// The ad server the repository ships, `ad-server.iabtl`, where it was
    /// `ad-server.mock`, and `[ad-server]`, where it was `[adserver]`.
    fn ad_server_names(&mut self) {
        if self.root().contains_key("adserver") {
            if self.root().contains_key("ad-server") {
                self.push_to_do(
                    "`[adserver]` and `[ad-server]` are both written. Keep `[ad-server]`",
                );
                return;
            }
            rename_key(self.root_mut(), "adserver", "ad-server");
            self.push_changed("renamed `[adserver]` to `[ad-server]`");
        }
        let Some(section) = self
            .root_mut()
            .get_mut("ad-server")
            .and_then(Item::as_table_mut)
        else {
            return;
        };
        let mut changed = Vec::new();
        if let Some(module) = section.get_mut("module") {
            let renamed = match module.as_str() {
                Some("mock") => Some("iabtl"),
                Some("ad-server.mock") => Some("ad-server.iabtl"),
                _ => None,
            };
            if let Some(new) = renamed {
                set_string(module, new);
                changed.push(format!(
                    "renamed the ad server `[ad-server] module` selects to `{new}`"
                ));
            }
        }
        if section.get("mock").is_some_and(Item::is_table) && !section.contains_key("iabtl") {
            rename_key(section, "mock", "iabtl");
            changed.push("renamed `[ad-server.mock]` to `[ad-server.iabtl]`".to_owned());
        }
        for name in keys_of(section) {
            let Some(implementation) = section
                .get_mut(&name)
                .and_then(Item::as_table_like_mut)
                .and_then(|table| table.get_mut("implementation"))
            else {
                continue;
            };
            if implementation.as_str() == Some("ad-server.mock") {
                set_string(implementation, "ad-server.iabtl");
                changed.push(format!(
                    "renamed the ad server `[ad-server.{name}] implementation` names to \
                     `ad-server.iabtl`"
                ));
            }
        }
        self.report.changed.extend(changed);
    }

    /// The `ads.txt` module `ads-txt.iabtl`, where it was `ads-txt.sellers`, in
    /// its section, its table and the `[[fetch]]` entries.
    fn ads_txt_names(&mut self) {
        let mut changed = Vec::new();
        if let Some(section) = self
            .root_mut()
            .get_mut("ads-txt")
            .and_then(Item::as_table_mut)
        {
            for key in ["module", "modules"] {
                let Some(item) = section.get_mut(key) else {
                    continue;
                };
                if rename_names(
                    item,
                    &[("sellers", "iabtl"), ("ads-txt.sellers", "ads-txt.iabtl")],
                ) {
                    changed.push(format!(
                        "renamed the ads.txt module `[ads-txt] {key}` selects to `iabtl`"
                    ));
                }
            }
            if section.get("sellers").is_some_and(Item::is_table) && !section.contains_key("iabtl")
            {
                rename_key(section, "sellers", "iabtl");
                changed.push("renamed `[ads-txt.sellers]` to `[ads-txt.iabtl]`".to_owned());
            }
        }
        for phase in ["fetch", "serve"] {
            let Some(entries) = self
                .root_mut()
                .get_mut(phase)
                .and_then(Item::as_array_of_tables_mut)
            else {
                continue;
            };
            for (index, entry) in entries.iter_mut().enumerate() {
                let Some(names) = entry.get_mut("middleware") else {
                    continue;
                };
                if rename_names(names, &[("ads-txt.sellers", "ads-txt.iabtl")]) {
                    changed.push(format!(
                        "renamed the middleware `ads-txt.sellers` to `ads-txt.iabtl` in \
                         `[[{phase}]]` entry {}",
                        index + 1
                    ));
                }
            }
        }
        self.report.changed.extend(changed);
    }

    /// `[tinybird]` into `[analytics.tinybird]`, selected by `[analytics]
    /// module` in place of its `enabled` line.
    fn tinybird(&mut self) {
        let table_path = path(&["tinybird"]);
        self.layout.make_table(&table_path);
        let Some(table) = self.layout.table_mut(&table_path) else {
            return;
        };
        let enabled = match table.get("enabled") {
            None => false,
            Some(item) => {
                if let Some(enabled) = item.as_bool() {
                    enabled
                } else {
                    self.push_to_do(
                        "`[tinybird] enabled` is not true or false, so it cannot say whether \
                         telemetry is sent",
                    );
                    return;
                }
            }
        };
        if self.root().contains_key("analytics") {
            self.push_to_do("`[tinybird]` and `[analytics]` are both written. Keep `[analytics]`");
            return;
        }
        if let Some(table) = self.layout.table_mut(&table_path) {
            table.remove("enabled");
        }
        let header = ["analytics", "tinybird"];
        if !enabled {
            self.layout.hold(&table_path, Some(&header[..]));
            self.push_changed(
                "commented out `[tinybird]` as `[analytics.tinybird]`, because it was not enabled",
            );
            return;
        }
        let Some(mut table) = self.layout.take(&table_path) else {
            return;
        };
        if self.ensure_section("analytics", table.position(), &prefix_of(&table)) {
            set_prefix(&mut table, "\n");
        }
        insert_at(self.root_mut(), &header, table);
        self.select("analytics", "tinybird");
        self.push_changed(
            "moved `[tinybird]` to `[analytics.tinybird]`, selected with `[analytics] module = \
             \"tinybird\"` in place of its `enabled` line",
        );
        self.push_check(
            "a telemetry row is now sent only for a reader whose permissions include \
             `analytics.ad_reporting.measure_ad_performance`",
        );
    }

    /// `include_mediator_response`, which the stack spells
    /// `include_adserver_response`.
    fn debug_options(&mut self) {
        let Some(options) = self
            .root_mut()
            .get_mut("debug")
            .and_then(|debug| debug.get_mut("auction_html_comment_options"))
            .and_then(Item::as_table_mut)
        else {
            return;
        };
        if options.contains_key("include_mediator_response")
            && !options.contains_key("include_adserver_response")
        {
            rename_key(
                options,
                "include_mediator_response",
                "include_adserver_response",
            );
            self.push_changed(
                "renamed `[debug.auction_html_comment_options] include_mediator_response` to \
                 `include_adserver_response`",
            );
        }
    }

    /// The lists of places `[consent]` held, which the stack reads from the
    /// publisher's permission policy file.
    fn consent_lists(&mut self) {
        let Some(consent) = self.root().get("consent") else {
            return;
        };
        let held: Vec<&str> = ["gdpr", "us_states"]
            .into_iter()
            .filter(|key| consent.get(key).is_some())
            .collect();
        for key in held {
            self.push_to_do(format!(
                "`[consent.{key}]` is not read. The places it covers are the `jurisdiction` \
                 values in the publisher's permission policy file, the one `[permissions] \
                 policy` names, so carry any change of yours there, then remove the table"
            ));
        }
    }

    /// A document that names no permission policy file, which no domain
    /// starts without. No policy ships with Trusted Server, so the command
    /// chooses none and leaves naming one to do.
    fn permission_policy(&mut self) {
        let named = self
            .root()
            .get("permissions")
            .and_then(|permissions| permissions.get("policy"))
            .and_then(Item::as_str)
            .is_some_and(|policy| !policy.is_empty());
        if !named {
            self.push_to_do(POLICY_TO_DO);
        }
    }

    /// A `template_cache_key_cookies` entry under the public prefix, which
    /// is one reader's own state.
    fn template_cookies(&mut self) {
        let prefix = PublicPrefix::default();
        let refused = self
            .root()
            .get("creative_opportunities")
            .and_then(|config| config.get("template_cache_key_cookies"))
            .and_then(Item::as_array)
            .is_some_and(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|name| prefix.owns_cookie_name(name.as_bytes()))
            });
        if refused {
            self.push_to_do(format!(
                "`[creative_opportunities] template_cache_key_cookies` names a cookie under the \
                 `{prefix}` prefix, which is one reader's own state and is refused now. Remove \
                 it"
            ));
        }
    }

    /// The host's location lookup main ran on every request, for a document
    /// written for main, on an adapter whose host has one. The Axum and Spin
    /// adapters refuse the selection, and main's lookup there located no
    /// request, so the document gets no geo module there. Cloudflare's
    /// lookup now reads the region, which main's did not, so the region
    /// rules of `permissions.yaml` can apply there, and a check line says so.
    fn main_defaults(&mut self) {
        if !self.adapter.has_location_lookup() || self.root().contains_key("geo") {
            return;
        }
        let mut geo = Table::new();
        geo.insert("module", value("platform"));
        self.root_mut().insert("geo", Item::Table(geo));
        self.push_changed(
            "selected `[geo] module = \"platform\"`, the host's location lookup main ran on \
             every request",
        );
        if self.adapter == Adapter::Cloudflare {
            self.push_check(
                "on Cloudflare the location lookup now reads the reader's region from \
                 `cf-region-code`, where Cloudflare sends it, and main's read none, so the \
                 region rules of `permissions.yaml`, such as a US state's `us-state` \
                 jurisdiction, can apply to a request now",
            );
        }
        let anchor = self
            .layout
            .subtree_end(&path(&["source"]))
            .or_else(|| self.layout.position_of(&path(&["publisher"])))
            .unwrap_or_else(|| self.layout.last_position());
        self.layout.place_after(anchor, &[path(&["geo"])]);
    }

    /// The location settings startup refuses and `ts config validate` does
    /// not check, being the host's own lookup on an adapter whose host has
    /// none, and an Edge Cookie module with no geo module unless `[geo]
    /// assume_single_jurisdiction` accepts that every request then takes the
    /// top of the `permissions.yaml` rules. Which way out to take is the
    /// operator's decision, so each is left to do.
    fn location_at_startup(&mut self) {
        let geo = self.root().get("geo");
        let module = geo
            .and_then(|geo| geo.get("module"))
            .and_then(Item::as_str)
            .map(str::to_owned);
        let acknowledged = geo
            .and_then(|geo| geo.get("assume_single_jurisdiction"))
            .and_then(Item::as_bool)
            == Some(true);
        let edge_cookie = self
            .root()
            .get("ec")
            .and_then(|ec| ec.get("module"))
            .and_then(Item::as_str)
            .is_some_and(|module| module != "none");
        let name = self.adapter.name();
        if module.as_deref() == Some("platform") && !self.adapter.has_location_lookup() {
            let acknowledge = if edge_cookie {
                " and set `[geo] assume_single_jurisdiction = true`, which accepts that every \
                 request takes the top of the `permissions.yaml` rules"
            } else {
                ""
            };
            self.push_to_do(format!(
                "`[geo] module = \"platform\"` selects the host's location lookup, and {name} has \
                 none, so startup refuses it. Select a geo module a linked crate offers, or remove \
                 the line{acknowledge}"
            ));
            return;
        }
        if edge_cookie && matches!(module.as_deref(), None | Some("none")) && !acknowledged {
            self.push_to_do(if self.adapter.has_location_lookup() {
                "`[ec]` selects an Edge Cookie module and `[geo]` selects no geo module, so every \
                 request takes the top of the `permissions.yaml` rules, which startup refuses \
                 unless `[geo] assume_single_jurisdiction = true` accepts it. Select a geo \
                 module, such as `platform`, or set that line"
                    .to_owned()
            } else {
                format!(
                    "`[ec]` selects an Edge Cookie module and no geo module is selected, because \
                     {name} has no location lookup of its own, so every request takes the top of \
                     the `permissions.yaml` rules, which startup refuses unless `[geo] \
                     assume_single_jurisdiction = true` accepts it. Set that line, or select a geo \
                     module a linked crate offers"
                )
            });
        }
    }

    /// The `[[fetch]]` and `[[serve]]` entries that place the page changes
    /// of the selected modules, for a document written for main, and the
    /// entry each file a module makes needs.
    fn page_entries(&mut self) {
        let entries = self.wanted_entries();
        let mut new: Vec<TablePath> = Vec::new();
        for wanted in entries {
            let PageEntry {
                phase,
                media_type,
                path: file,
                middleware,
            } = wanted;
            let mut entry = Table::new();
            entry.insert("media_type", value(media_type));
            if let Some(file) = file {
                entry.insert("path", value(file));
            }
            entry.insert("middleware", value(name_list(&middleware)));
            set_prefix(
                &mut entry,
                if new.is_empty() {
                    ENTRIES_COMMENT
                } else {
                    "\n"
                },
            );
            let array = self
                .root_mut()
                .entry(phase)
                .or_insert_with(|| Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
            let Some(array) = array.as_array_of_tables_mut() else {
                continue;
            };
            array.push(entry);
            new.push(vec![
                Step::Key(phase.to_owned()),
                Step::Index(array.len() - 1),
            ]);
            self.report.changed.push(match file {
                Some(file) => format!(
                    "added a `[[{phase}]]` entry for `{media_type}` on `{file}` that names `{}`",
                    middleware.join("`, `")
                ),
                None => format!(
                    "added a `[[{phase}]]` entry for `{media_type}` that names the middleware of \
                     the selected modules"
                ),
            });
        }
        if new.is_empty() {
            return;
        }
        let anchor = self.layout.last_position();
        self.layout.place_after(anchor, &new);
    }

    /// The page entries the document lacks: one for each phase and media
    /// type the selected modules supply middleware for, in the order the
    /// builders supply it, for a document written for main, whose modules
    /// changed every page, and one for each file a selected module makes
    /// that no entry places, which startup refuses. A document in the
    /// stack's shape loads without page entries, so they are left to the
    /// operator there.
    fn wanted_entries(&self) -> Vec<PageEntry> {
        let selected = self.selected_ids();
        let supplied = supplied_middleware(&crate::app_config::linked_builders());
        let mut entries = Vec::new();
        for phase in MiddlewarePhase::ALL {
            let mut media_types: Vec<&'static str> = Vec::new();
            for (id, middleware) in &supplied {
                if middleware.phase == phase
                    && selected.contains(id)
                    && !media_types.contains(&middleware.media_type)
                {
                    media_types.push(middleware.media_type);
                }
            }
            media_types.sort_by_key(|media_type| *media_type != HTML_MEDIA_TYPE);
            for media_type in media_types {
                let names: Vec<&'static str> = supplied
                    .iter()
                    .filter(|(id, middleware)| {
                        middleware.phase == phase
                            && middleware.media_type == media_type
                            && selected.contains(id)
                    })
                    .map(|(_, middleware)| middleware.name)
                    .collect();
                let mut rest = Vec::new();
                for name in names {
                    match FILE_MIDDLEWARE.iter().find(|(file, _)| *file == name) {
                        Some((_, file)) => {
                            if !self.names_middleware(name) {
                                entries.push(PageEntry {
                                    phase: phase.as_str(),
                                    media_type,
                                    path: Some(*file),
                                    middleware: vec![name],
                                });
                            }
                        }
                        None => rest.push(name),
                    }
                }
                if self.for_main && !rest.is_empty() {
                    entries.push(PageEntry {
                        phase: phase.as_str(),
                        media_type,
                        path: None,
                        middleware: rest,
                    });
                }
            }
        }
        entries.sort_by_key(|entry| entry.phase != MiddlewarePhase::Fetch.as_str());
        entries
    }

    /// What main did on every request that no setting expresses now, for a
    /// document written for main.
    fn main_checks(&mut self) {
        self.push_check(
            "no response carries `x-ts-version` or the platform's version now. Name \
             `service_version` in the `[[serve]]` entry to write `x-ts-version` and \
             `x-ts-service-version` on the pages it covers",
        );
        if !self.selected_ids().contains(&"nextjs") {
            self.push_check(
                "core no longer moves the origin's address in a React Server Components Flight \
                 response (`text/x-component`). A Next.js App Router site selects `[framework] \
                 module = \"nextjs\"`, which adds its page changes, and names \
                 `framework.nextjs.flight` in a `[[fetch]]` entry for `text/x-component`",
            );
        }
        self.push_check(self.geo_headers_check());
    }

    /// What became of the location headers main wrote on every response on
    /// the adapter. `headers.geo` is never selected for the operator,
    /// because it writes the reader's location.
    fn geo_headers_check(&self) -> String {
        match self.adapter {
            Adapter::Fastly => "no response carries an `x-geo-*` header now, where main wrote the \
                                reader's location in them on every response. To keep them, \
                                select `[headers] modules = [\"geo\"]` and name `headers.geo` in a \
                                `[[serve]]` entry for the pages that are to carry them"
                .to_owned(),
            Adapter::Cloudflare => "no response carries `x-geo-info-available` now, the one \
                                    `x-geo-*` header main wrote on Cloudflare. `headers.geo` \
                                    would write it again, and with it the reader's location in \
                                    the other `x-geo-*` headers, which main never sent on \
                                    Cloudflare"
                .to_owned(),
            Adapter::Axum | Adapter::Spin => format!(
                "no response carries `x-geo-info-available` now, which main wrote as `false` on \
                 every response on {}, where it located no request",
                self.adapter.name()
            ),
        }
    }

    /// The section and the name a section selects the module that replaced
    /// main's integration `id` by.
    fn target(&self, id: &str) -> Option<(&'static str, &'static str)> {
        self.modules
            .iter()
            .find(|module| module.id == id)
            .map(|module| (module.section, module.written))
    }

    /// The ids of the modules the document selects, core's `robots.txt`
    /// middleware with `[robots-txt]` among them.
    fn selected_ids(&self) -> Vec<&'static str> {
        let root = self.root();
        let mut ids: Vec<&'static str> = self
            .modules
            .iter()
            .filter(|module| {
                root.get(module.section).is_some_and(|section| {
                    selection(section)
                        .iter()
                        .any(|name| name == module.written || name == module.full)
                })
            })
            .map(|module| module.id)
            .collect();
        for &(id, vendor) in &self.shared {
            if root
                .get("shared")
                .is_some_and(|shared| shared.get(vendor).is_some())
            {
                ids.push(id);
            }
        }
        if root.contains_key("robots-txt") {
            ids.push(trusted_server_core::robots_txt::middleware::MODULE);
        }
        ids
    }

    /// Whether a `[[fetch]]` or `[[serve]]` entry names `middleware`.
    fn names_middleware(&self, middleware: &str) -> bool {
        ["fetch", "serve"].into_iter().any(|phase| {
            self.root()
                .get(phase)
                .and_then(Item::as_array_of_tables)
                .is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry
                            .get("middleware")
                            .and_then(Item::as_array)
                            .is_some_and(|names| {
                                names.iter().any(|name| name.as_str() == Some(middleware))
                            })
                    })
                })
        })
    }

    /// Makes `section` a table that writes its header, at `position` and
    /// with `prefix` ahead of it when it is made now, and says whether it
    /// took them.
    fn ensure_section(&mut self, section: &str, position: Option<isize>, prefix: &str) -> bool {
        let root = self.root_mut();
        if let Some(existing) = root.get_mut(section) {
            let Some(table) = existing.as_table_mut() else {
                return false;
            };
            if !table.is_implicit() {
                return false;
            }
            table.set_implicit(false);
            if let Some(position) = position {
                table.set_position(position);
            }
            set_prefix(table, prefix);
            return true;
        }
        let mut table = Table::new();
        if let Some(position) = position {
            table.set_position(position);
        }
        set_prefix(&mut table, prefix);
        root.insert(section, Item::Table(table));
        true
    }

    /// Selects `written` in `section`, in the order the builders list the
    /// modules, and gives the key the selection is written under.
    fn select(&mut self, section: &str, written: &'static str) -> &'static str {
        let ranks: Vec<(&'static str, usize)> = self
            .modules
            .iter()
            .enumerate()
            .filter(|(_, module)| module.section == section)
            .map(|(rank, module)| (module.written, rank))
            .collect();
        let rank = |name: &str| {
            ranks
                .iter()
                .find(|(written, _)| *written == name)
                .map_or(usize::MAX, |(_, rank)| *rank)
        };
        let Some(table) = self
            .root_mut()
            .get_mut(section)
            .and_then(Item::as_table_mut)
        else {
            return "module";
        };
        if let Some(modules) = table.get_mut("modules").and_then(Item::as_array_mut) {
            if !modules.iter().any(|name| name.as_str() == Some(written)) {
                let index = modules
                    .iter()
                    .position(|name| name.as_str().is_some_and(|name| rank(name) > rank(written)))
                    .unwrap_or(modules.len());
                modules.insert(index, written);
                modules.fmt();
            }
            return "modules";
        }
        if let Some(existing) = table
            .get("module")
            .and_then(Item::as_str)
            .map(str::to_owned)
        {
            if existing == written {
                return "module";
            }
            let mut names = vec![existing.as_str(), written];
            names.sort_by_key(|name| rank(name));
            rename_key(table, "module", "modules");
            if let Some(item) = table.get_mut("modules") {
                let names: Array = names.into_iter().collect();
                *item = value(names);
            }
            return "modules";
        }
        if LIST_SECTIONS.contains(&section) {
            let names: Array = std::iter::once(written).collect();
            insert_first(table, "modules", value(names));
            "modules"
        } else {
            insert_first(table, "module", value(written));
            "module"
        }
    }
}

/// Whether `root` was written for main: it has `[publisher]`, and every
/// table at its top level is one main read.
fn written_for_main(root: &Table) -> bool {
    root.contains_key("publisher") && root.iter().all(|(key, _)| MAIN_TABLES.contains(&key))
}

/// The implementation a main `profile` names, by module path.
fn implementation_of(profile: &str) -> Option<&'static str> {
    match profile {
        "standard" => Some("auction-protocol.openrtb"),
        "prebid-server" => Some("auction.prebid-server"),
        "aps" => Some("auction.aps"),
        _ => None,
    }
}

/// Whether every path a `[[handlers]]` pattern matches is one this release
/// closes, with no alternative outside a group that could match another.
///
/// Main matched a pattern anywhere in the path, so the pattern must be
/// anchored. Every path that starts with `/_ts/admin` is closed, but of
/// `/admin/keys` only the address itself and the paths below `/admin/keys/`
/// are, so that prefix must be followed by a `/` or the end of the path, and
/// `^/admin/keys` alone, which also matches `/admin/keystore`, is not.
/// `?` and `*` let what comes before them match no times, and so can a
/// count in `{}`, so none of the three may follow `^/_ts/admin` or that
/// bound, as `^/_ts/admin?` also matches `/_ts/admit` and `^/admin/keys/?`
/// matches `/admin/keystore`. The text after the prefix or the bound may
/// hold no `[` and no `#` either, because a character class, or a comment
/// the `(?x)` flag allows, can hide a `|` from
/// [`has_top_level_alternation`], as the `(` in `[](]` is a character and
/// not a group.
fn guards_only_closed_paths(pattern: &str) -> bool {
    const KEYS_BOUNDS: &[&str] = &["/", "$", "(?:/|$)", "(/|$)"];
    let rest = match pattern.strip_prefix("^/admin/keys") {
        Some(rest) => KEYS_BOUNDS
            .iter()
            .find_map(|bound| rest.strip_prefix(bound)),
        None => pattern.strip_prefix("^/_ts/admin"),
    };
    rest.is_some_and(|rest| !rest.starts_with(['?', '*', '{']) && !rest.contains(['[', '#']))
        && !has_top_level_alternation(pattern)
}

/// Whether `pattern` has a `|` outside every group.
///
/// It reads escapes and groups, and no character class or comment, which
/// its caller refuses.
fn has_top_level_alternation(pattern: &str) -> bool {
    let mut depth = 0_usize;
    let mut characters = pattern.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                characters.next();
            }
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '|' if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

/// The names a section selects, from `module` or `modules`.
fn selection(section: &Item) -> Vec<String> {
    if let Some(module) = section.get("module").and_then(Item::as_str) {
        return vec![module.to_owned()];
    }
    section
        .get("modules")
        .and_then(Item::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The keys of `table`, in order.
fn keys_of(table: &Table) -> Vec<String> {
    table.iter().map(|(key, _)| key.to_owned()).collect()
}

/// The keys of `table` among `wanted`, in the order the table holds them.
fn keys_in_order<'a>(table: &Table, wanted: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let wanted: Vec<&str> = wanted.collect();
    table
        .iter()
        .filter_map(|(key, _)| wanted.iter().find(|name| **name == key).copied())
        .collect()
}

/// `key` under the name `name`, with the comments and spacing written
/// around it.
fn renamed(key: &Key, name: &str) -> Key {
    Key::new(name).with_leaf_decor(key.leaf_decor().clone())
}

/// Renames `old` to `new` in `table`, keeping its place and its comments.
fn rename_key(table: &mut Table, old: &str, new: &str) {
    let entries: Vec<(Key, Item)> = keys_of(table)
        .iter()
        .filter_map(|key| table.remove_entry(key))
        .collect();
    for (key, item) in entries {
        if key.get() == old {
            table.insert_formatted(&renamed(&key, new), item);
        } else {
            table.insert_formatted(&key, item);
        }
    }
}

/// Puts `item` under `key` ahead of every other entry of `table`.
fn insert_first(table: &mut Table, key: &str, item: Item) {
    insert_first_formatted(table, &Key::new(key), item);
}

/// Puts `item` under `key`, as formatted, ahead of every other entry.
fn insert_first_formatted(table: &mut Table, key: &Key, item: Item) {
    let entries: Vec<(Key, Item)> = keys_of(table)
        .iter()
        .filter_map(|name| table.remove_entry(name))
        .collect();
    table.insert_formatted(key, item);
    for (key, item) in entries {
        table.insert_formatted(&key, item);
    }
}

/// Puts `table` at `parts` below `root`, making the tables on the way as
/// tables that write no header.
fn insert_at(root: &mut Table, parts: &[&str], table: Table) {
    let Some((last, parents)) = parts.split_last() else {
        return;
    };
    let mut parent = root;
    for part in parents {
        let entry = parent.entry(part).or_insert_with(|| {
            let mut implicit = Table::new();
            implicit.set_implicit(true);
            Item::Table(implicit)
        });
        let Some(next) = entry.as_table_mut() else {
            return;
        };
        parent = next;
    }
    parent.insert(last, Item::Table(table));
}

/// Moves the settings of `config`, a provider's `profile_config`, into the
/// provider's own table, after its own, with the comments written above
/// `config` ahead of the first of them.
fn flatten_into(table: &mut Table, config: Item) {
    let Ok(mut config) = config.into_table() else {
        return;
    };
    let mut prefix = Some(prefix_of(&config));
    for name in keys_of(&config) {
        let Some((mut key, item)) = config.remove_entry(&name) else {
            continue;
        };
        if item.is_value()
            && let Some(prefix) = prefix.take()
        {
            let own = key
                .leaf_decor()
                .prefix()
                .and_then(RawString::as_str)
                .unwrap_or("")
                .to_owned();
            key.leaf_decor_mut().set_prefix(format!("{prefix}{own}"));
        }
        table.insert_formatted(&key, item);
    }
}

/// Sets the string `item` holds to `text`, keeping its comments.
fn set_string(item: &mut Item, text: &str) {
    if let Some(found) = item.as_value_mut() {
        set_value_string(found, text);
    }
}

/// Sets the string `found` holds to `text`, keeping its comments.
fn set_value_string(found: &mut Value, text: &str) {
    let decor = found.decor().clone();
    *found = Value::from(text);
    *found.decor_mut() = decor;
}

/// Renames the names `item` holds, a name or a list of names, and says
/// whether any changed.
fn rename_names(item: &mut Item, renames: &[(&str, &str)]) -> bool {
    let mut changed = false;
    if let Some(name) = item.as_value_mut() {
        if let Some(array) = name.as_array_mut() {
            for name in array.iter_mut() {
                if let Some((_, new)) = renames.iter().find(|(old, _)| name.as_str() == Some(*old))
                {
                    set_value_string(name, new);
                    changed = true;
                }
            }
        } else if let Some((_, new)) = renames.iter().find(|(old, _)| name.as_str() == Some(*old)) {
            set_value_string(name, new);
            changed = true;
        }
    }
    changed
}

/// The names of a page entry's `middleware`, one to a line where there are
/// several.
fn name_list(names: &[&str]) -> Array {
    let mut array: Array = names.iter().copied().collect();
    if names.len() > 1 {
        for name in array.iter_mut() {
            name.decor_mut().set_prefix("\n  ");
        }
        array.set_trailing_comma(true);
        array.set_trailing("\n");
    }
    array
}
