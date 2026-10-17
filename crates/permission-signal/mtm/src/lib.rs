//! Model Terms for Marketing (MTM) as a permission signal module.
//!
//! The Preference Management Platform (PMP) asks a visitor one question and
//! keeps one of three words, `standard`, `personalized` or `non-marketing`.
//! That word is the whole of the answer, and this module reads it from the
//! first party cookie `__mtm_pref` the PMP writes on the publisher's domain,
//! which exists so that the server can see the answer at all. The PMP keeps
//! its own copy elsewhere and mirrors it here. Absent, the module says
//! nothing and the country and region rules stand, which is clause 5.3 of the
//! Model Terms, under which a preference with no value permits neither kind of
//! marketing.
//!
//! What each word means is worked out from the Model Terms' own text, the
//! versioned document this module declares as the terms the data is
//! available under. Appendix 1 defines standard marketing as content unrelated
//! to browsing history or interactions, and personalized marketing as content
//! related to them, both including the use of cookies, and clause 4.4 permits
//! measurement, optimization and product development subject to the
//! preference. So the Data Uses that depend on browsing history or
//! interactions are the ones `standard` and `personalized` differ on.
//!
//! `non-marketing` changes nothing. The visitor named neither kind of
//! marketing, so this module has no choice to act on and every Data Use is
//! left to the country and region rules, which is clause 5.3's position.
//!
//! The word is still recorded and the terms are still declared, because the
//! question was answered and that is a different fact from never having been
//! asked. Only the permissions are untouched.
//!
//! Where a baseline grants marketing without a signal, the baseline therefore
//! applies. Under the shipped `gdpr-eu` and `gdpr-uk` groups every marketing
//! Data Use requires a signal, so nothing is granted. Under `us-opt-out` they
//! are granted outright, so marketing is permitted. A deployment wanting a
//! decline to bind in an opt-out jurisdiction says so in its own permissions
//! policy, which is where jurisdiction belongs.
//!
//! This module lives outside `trusted-server-core` deliberately, like every
//! scheme. Why is set out once, in `permission_signal/README.md` in core.

use std::sync::OnceLock;

use trusted_server_core::constants::COOKIE_MTM_PREF;
use trusted_server_core::evidence::RequestInfo;
use trusted_server_core::module_context::ModuleCall;
use trusted_server_core::permission_signal::{PermissionSignalModule, SignalInput};
use trusted_server_core::permissions::{
    ConsentSignal, Permission, PermissionSet, SignalPolicy, ValidSignal,
};
use trusted_server_core::tdl::Tdl;

/// The name `[permission-signal] modules` selects this module by, from its
/// crate folder.
#[must_use]
pub fn name() -> &'static str {
    trusted_server_core::module_name!()
}

/// The name the page is told a signal came from, being the name without the
/// type folder.
fn short() -> &'static str {
    trusted_server_core::module_name::short_form(
        trusted_server_core::permission_signal::MODULE_TYPE,
        name(),
    )
}

/// The scheme, as a [`ValidSignal`] names it.
pub const SCHEME: &str = "mtm";

/// The terms the data is available under when a word is present. Versioned,
/// and never a page that can be edited, because whoever receives the data
/// has to be able to prove what terms were in force when it was sent.
pub const TERMS: &str = "https://m4ow.uk/mtm/2.txt";

/// The Data Uses either word grants: storage and the cookies both kinds of
/// marketing include, contextual advertising, and the measurement,
/// optimization and product development clause 4.4 permits.
const UNDER_EITHER_WORD: &[&str] = &[
    "necessary.operations.storage",
    "advertising_marketing.first_party.contextual",
    "analytics.ad_reporting.measure_ad_performance",
    "analytics.ad_reporting.content_performance",
    "analytics.ad_reporting.market_research",
    "necessary.operations.improve",
    "select-basic-content",
];

/// The Data Uses that depend on browsing history or interactions, granted by
/// `personalized` and refused by `standard`. Refused rather than left alone,
/// because the visitor chose against them and that choice has to stand over
/// a country baseline that would otherwise grant them.
const PERSONALIZED_ONLY: &[&str] = &[
    "advertising_marketing.profiling",
    "advertising_marketing.first_party.targeted",
    "advertising_marketing.personalize.profiling",
    "advertising_marketing.personalize.content",
];

/// The three words a PMP answer can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preference {
    /// Marketing unrelated to browsing history or interactions.
    Standard,
    /// Marketing related to browsing history or interactions.
    Personalized,
    /// No marketing at all. The visitor actively declined it.
    NonMarketing,
}

impl Preference {
    /// The word as the PMP wrote it, or `None` for anything else. Any other
    /// value is not a word this scheme knows and is treated as absent.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim() {
            "standard" => Some(Self::Standard),
            "personalized" => Some(Self::Personalized),
            "non-marketing" => Some(Self::NonMarketing),
            _ => None,
        }
    }

    /// The word as the PMP writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Personalized => "personalized",
            Self::NonMarketing => "non-marketing",
        }
    }
}

/// The answer on this request, read from the platform's first party cookie.
#[must_use]
pub fn preference(evidence: &dyn RequestInfo) -> Option<Preference> {
    evidence.cookie(COOKIE_MTM_PREF).and_then(Preference::parse)
}

/// The Model Terms for Marketing, read from the PMP answer.
#[derive(Debug, Default, Clone, Copy)]
pub struct MtmModule;

impl MtmModule {
    /// A new module.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The word in the request's evidence, when one is present.
    fn read_signal(&self, evidence: &dyn RequestInfo) -> Option<ValidSignal> {
        preference(evidence).map(|word| ValidSignal::new(short(), SCHEME, word.as_str()))
    }

    /// The Model Terms, when the request's evidence carries a word.
    fn read_tdls(&self, evidence: &dyn RequestInfo) -> Vec<Tdl> {
        if preference(evidence).is_none() {
            return Vec::new();
        }
        Tdl::new(TERMS).into_iter().collect()
    }
}

/// Every Data Use either word says something about, computed once.
fn covered() -> PermissionSet {
    static COVERED: OnceLock<PermissionSet> = OnceLock::new();
    *COVERED.get_or_init(|| {
        Permission::all()
            .filter(|permission| {
                let name = permission.as_str();
                UNDER_EITHER_WORD.contains(&name) || PERSONALIZED_ONLY.contains(&name)
            })
            .collect()
    })
}

impl PermissionSignalModule for MtmModule {
    fn id(&self) -> &'static str {
        name()
    }

    fn signal(&self, permission: Permission, input: &SignalInput<'_>) -> ConsentSignal {
        let Some(word) = preference(input.evidence) else {
            // No answer on the request. Silence, so the country and region
            // rules stand, amended by whatever other modules say.
            return ConsentSignal::Neutral;
        };
        // `non-marketing` changes nothing. The visitor made no marketing choice
        // for this module to act on, so whatever the country and region
        // rules say stands, which is clause 5.3's position for a preference
        // that names neither kind of marketing.
        //
        // What this means where a baseline grants marketing without a signal,
        // stated because it is not obvious and somebody will need it. Under
        // the shipped `gdpr-eu` and `gdpr-uk` groups every marketing Data Use
        // is `requires_signal`, so nothing is granted and the answer is a
        // refusal in effect. Under `us-opt-out` the same Data Uses are
        // `granted`, so the baseline applies and marketing is permitted. A
        // deployment that wants a decline to bind in an opt-out jurisdiction
        // sets that in its own permissions policy, which is where jurisdiction
        // belongs, and not in this module, which only says what the two
        // marketing words mean.
        if word == Preference::NonMarketing {
            return ConsentSignal::Neutral;
        }
        let name = permission.as_str();
        if UNDER_EITHER_WORD.contains(&name) {
            return ConsentSignal::Grant;
        }
        if PERSONALIZED_ONLY.contains(&name) {
            return match word {
                Preference::Personalized => ConsentSignal::Grant,
                // Revoked rather than left alone, because the visitor chose
                // against these and a choice must stand over a country
                // baseline that would otherwise grant them.
                Preference::Standard => ConsentSignal::Revoke,
                // Unreachable: handled above.
                Preference::NonMarketing => ConsentSignal::Neutral,
            };
        }
        // A Data Use the Model Terms say nothing about.
        ConsentSignal::Neutral
    }

    /// Every Data Use either word grants, which is what a page waits for
    /// while the PMP question is open. The policy is not consulted, because
    /// what the words mean is the Model Terms' own.
    fn grants(&self, _policy: &SignalPolicy) -> PermissionSet {
        covered()
    }

    /// The word, when one is present. There is nothing else to validate,
    /// because a value that is not one of the two words is not an answer.
    fn valid_signal(&self, call: ModuleCall<'_>) -> Option<ValidSignal> {
        call.inject(self, Self::read_signal).ok().flatten()
    }

    /// The Model Terms, whenever a word is present, because either answer is
    /// given under them. Nothing is declared without an answer.
    fn tdls(&self, call: ModuleCall<'_>) -> Vec<Tdl> {
        call.inject(self, Self::read_tdls).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;

    use super::*;
    use trusted_server_core::consent::ConsentContext;
    use trusted_server_core::evidence::OwnedRequestInfo;
    use trusted_server_core::permissions::Acquisition;

    fn with_cookie(value: &str) -> OwnedRequestInfo {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            format!("other=1; {COOKIE_MTM_PREF}={value}; another=2")
                .parse()
                .expect("should build a cookie header"),
        );
        OwnedRequestInfo::new(String::new(), headers)
    }

    fn answer(evidence: &OwnedRequestInfo, permission: Permission) -> ConsentSignal {
        let consent = ConsentContext::default();
        let policy = SignalPolicy::default();
        let input = SignalInput::new(&consent, evidence, &policy, Acquisition::RequiresSignal);
        MtmModule::new().signal(permission, &input)
    }

    fn named(name: &str) -> Permission {
        Permission::all()
            .find(|permission| permission.as_str() == name)
            .unwrap_or_else(|| panic!("the taxonomy should carry {name}"))
    }

    /// The exact bytes the Preference Management Platform writes, generated
    /// by running its own code rather than transcribed from a document. Both
    /// implementations are checked against the same string so the contract
    /// between them is proven rather than agreed twice from two readings.
    ///
    /// A browser sends only the name and value, so the value is taken the way
    /// a browser takes it, which also proves no attribute can leak into it.
    const PMP_SET_COOKIE: &[(&str, &str)] = &[
        (
            "standard",
            "__mtm_pref=standard; Path=/; Max-Age=34560000; SameSite=Lax; Secure",
        ),
        (
            "personalized",
            "__mtm_pref=personalized; Path=/; Max-Age=34560000; SameSite=Lax; Secure",
        ),
        (
            "non-marketing",
            "__mtm_pref=non-marketing; Path=/; Max-Age=34560000; SameSite=Lax; Secure",
        ),
        (
            "personalized",
            "__mtm_pref=personalized; Path=/; Max-Age=34560000; SameSite=Lax; Domain=.example.com; Secure",
        ),
        (
            "non-marketing",
            "__mtm_pref=non-marketing; Path=/; Max-Age=34560000; SameSite=Lax; Domain=.example.com; Secure",
        ),
    ];

    /// The value a browser would send back, taken from a `Set-Cookie` string.
    fn value_a_browser_would_send(set_cookie: &str) -> &str {
        let pair = set_cookie.split(';').next().unwrap_or_default().trim();
        pair.split_once('=').expect("a cookie is name=value").1
    }

    /// Every string the platform writes must parse to the word it names, with
    /// no attribute leaking into the value and no whitespace surviving.
    #[test]
    fn the_platforms_own_set_cookie_strings_parse_to_the_word_they_carry() {
        for (word, set_cookie) in PMP_SET_COOKIE {
            let value = value_a_browser_would_send(set_cookie);
            assert_eq!(
                value, *word,
                "the value a browser sends must be the bare word"
            );
            assert_eq!(
                Preference::parse(value).map(Preference::as_str),
                Some(*word),
                "the module must accept what the platform writes: {set_cookie}",
            );
        }
    }

    /// The cookie has to stay readable by the script that owns the question.
    ///
    /// A browser lets a script neither read nor replace a cookie carrying
    /// `HttpOnly` (RFC 6265 section 5.3), so a platform that set one would stop
    /// seeing its own answer and the visitor's next choice would be lost with
    /// nothing reporting anything wrong.
    ///
    /// This module only ever reads the cookie, so the rule binds whoever
    /// writes it. The assertion is here because these fixtures are the
    /// platform's real output, so the day that changes, this is where it shows.
    #[test]
    fn the_preference_cookie_is_never_httponly() {
        for (word, set_cookie) in PMP_SET_COOKIE {
            assert!(
                !set_cookie.to_ascii_lowercase().contains("httponly"),
                "`{word}` must stay readable by the script that owns it: {set_cookie}",
            );
        }
    }

    /// The permission each word produces, asserted as a whole set rather than
    /// row by row, so the contract can be read off one place and any future
    /// change to the table has to be made deliberately.
    #[test]
    fn each_word_produces_the_permissions_the_model_terms_table_states() {
        let marketing_only = |word: &str| -> (Vec<&'static str>, Vec<&'static str>) {
            let evidence = with_cookie(word);
            let mut granted = Vec::new();
            let mut revoked = Vec::new();
            for name in UNDER_EITHER_WORD.iter().chain(PERSONALIZED_ONLY.iter()) {
                match answer(&evidence, named(name)) {
                    ConsentSignal::Grant => granted.push(*name),
                    ConsentSignal::Revoke => revoked.push(*name),
                    // `non-marketing` leaves everything to the baseline, so
                    // Neutral is the whole of its answer and is counted
                    // neither way.
                    ConsentSignal::Neutral => {}
                }
            }
            granted.sort_unstable();
            revoked.sort_unstable();
            (granted, revoked)
        };

        let mut personalized_only_sorted = PERSONALIZED_ONLY.to_vec();
        personalized_only_sorted.sort_unstable();

        let (standard_grant, standard_revoke) = marketing_only("standard");
        assert_eq!(standard_revoke, personalized_only_sorted);
        assert_eq!(standard_grant.len(), UNDER_EITHER_WORD.len());

        let (personalized_grant, personalized_revoke) = marketing_only("personalized");
        assert!(
            personalized_revoke.is_empty(),
            "personalized refuses nothing"
        );
        assert_eq!(
            personalized_grant.len(),
            UNDER_EITHER_WORD.len() + PERSONALIZED_ONLY.len(),
        );

        // `non-marketing` moves nothing in either direction, so both lists are
        // empty and the country and region rules decide every one of them.
        let (decline_grant, decline_revoke) = marketing_only("non-marketing");
        assert!(
            decline_grant.is_empty(),
            "a decline grants nothing: {decline_grant:?}"
        );
        assert!(
            decline_revoke.is_empty(),
            "a decline refuses nothing: {decline_revoke:?}"
        );
    }

    #[test]
    fn answers_to_its_identifier() {
        assert_eq!(
            MtmModule::new().id(),
            "permission-signal.mtm",
            "the module is named by its crate folder"
        );
    }

    #[test]
    fn reads_exactly_the_three_words_and_treats_anything_else_as_absent() {
        assert_eq!(Preference::parse("standard"), Some(Preference::Standard));
        assert_eq!(
            Preference::parse(" personalized "),
            Some(Preference::Personalized)
        );
        assert_eq!(
            Preference::parse("non-marketing"),
            Some(Preference::NonMarketing)
        );
        assert_eq!(
            Preference::parse("non_marketing"),
            None,
            "the PMP writes the hyphen, and a near miss is not an answer"
        );
        assert_eq!(
            Preference::parse("Personalised"),
            None,
            "not a word the scheme knows"
        );
        assert_eq!(Preference::parse(""), None);
        assert_eq!(
            preference(&with_cookie("standard")),
            Some(Preference::Standard)
        );
        assert_eq!(preference(&with_cookie("yes")), None);
        assert_eq!(preference(&OwnedRequestInfo::default()), None);
    }

    #[test]
    fn either_word_grants_what_both_kinds_of_marketing_include() {
        for name in UNDER_EITHER_WORD {
            for word in ["standard", "personalized"] {
                assert_eq!(
                    answer(&with_cookie(word), named(name)),
                    ConsentSignal::Grant,
                    "{word} should grant {name}"
                );
            }
        }
    }

    #[test]
    fn only_personalized_grants_what_depends_on_browsing_history_and_standard_refuses_it() {
        for name in PERSONALIZED_ONLY {
            assert_eq!(
                answer(&with_cookie("personalized"), named(name)),
                ConsentSignal::Grant,
                "personalized should grant {name}"
            );
            assert_eq!(
                answer(&with_cookie("standard"), named(name)),
                ConsentSignal::Revoke,
                "standard should refuse {name}, because the visitor chose against it"
            );
        }
    }

    #[test]
    fn says_nothing_about_a_data_use_the_terms_do_not_cover() {
        let email = named("advertising_marketing.communications.email");
        assert_eq!(
            answer(&with_cookie("personalized"), email),
            ConsentSignal::Neutral
        );
        assert_eq!(
            answer(&with_cookie("standard"), email),
            ConsentSignal::Neutral
        );
    }

    #[test]
    fn says_nothing_at_all_without_a_word() {
        for permission in Permission::all() {
            assert_eq!(
                answer(&OwnedRequestInfo::default(), permission),
                ConsentSignal::Neutral,
                "no answer should leave {} to the country rules",
                permission.as_str()
            );
            assert_eq!(
                answer(&with_cookie("maybe"), permission),
                ConsentSignal::Neutral,
                "a value that is not a word should be treated as absent"
            );
        }
    }

    #[test]
    fn declares_the_eleven_data_uses_the_words_cover() {
        let declared = MtmModule::new().grants(&SignalPolicy::default());
        assert_eq!(
            declared.iter().count(),
            UNDER_EITHER_WORD.len() + PERSONALIZED_ONLY.len(),
            "should declare each covered Data Use once"
        );
        for name in UNDER_EITHER_WORD.iter().chain(PERSONALIZED_ONLY) {
            assert!(declared.contains(named(name)), "{name} should be declared");
        }
    }

    #[test]
    fn vouches_for_the_word_and_declares_the_terms_only_when_one_is_present() {
        let module = MtmModule::new();
        assert_eq!(
            module.read_signal(&with_cookie("standard")),
            Some(ValidSignal::new(short(), SCHEME, "standard"))
        );
        let declared = module.read_tdls(&with_cookie("personalized"));
        assert_eq!(
            declared.iter().map(Tdl::as_str).collect::<Vec<_>>(),
            vec![TERMS],
            "an answer is given under the Model Terms"
        );
        assert_eq!(module.read_signal(&with_cookie("maybe")), None);
        assert!(module.read_tdls(&with_cookie("maybe")).is_empty());
        assert_eq!(module.read_signal(&OwnedRequestInfo::default()), None);
        assert!(module.read_tdls(&OwnedRequestInfo::default()).is_empty());
    }

    /// `non-marketing` changes nothing, so every Data Use the Model Terms
    /// cover comes back Neutral and the country and region rules decide.
    #[test]
    fn non_marketing_changes_nothing_at_all() {
        let evidence = with_cookie("non-marketing");
        for name in UNDER_EITHER_WORD.iter().chain(PERSONALIZED_ONLY.iter()) {
            assert_eq!(
                answer(&evidence, named(name)),
                ConsentSignal::Neutral,
                "non-marketing must leave {name} to the baseline",
            );
        }
    }

    /// The word is still an answer even though it moves no permission, so it
    /// is recorded and the terms are declared. A reader looking at what the
    /// appliance decided can then see that the question was answered, which is
    /// a different fact from never having been asked.
    #[test]
    fn non_marketing_is_still_a_recorded_answer_under_the_terms() {
        let evidence = with_cookie("non-marketing");
        let module = MtmModule::new();

        let signal = module
            .read_signal(&evidence)
            .expect("a decline is a valid answer");
        assert_eq!(signal.value, "non-marketing");
        assert_eq!(
            module.read_tdls(&evidence).len(),
            1,
            "the answer was given under the Model Terms whatever it said",
        );
    }

    #[test]
    fn neither_word_is_a_withdrawal() {
        let consent = ConsentContext::default();
        let policy = SignalPolicy::default();
        let evidence = with_cookie("standard");
        let input = SignalInput::new(&consent, &evidence, &policy, Acquisition::RequiresSignal);
        assert!(
            !MtmModule::new().withdraws(Permission::StoreOnDevice, &input),
            "a standard answer refuses personalization and leaves an issued identifier alone"
        );
    }
}
