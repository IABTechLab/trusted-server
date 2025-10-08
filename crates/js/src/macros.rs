macro_rules! count_variants {
    ($($variant:ident),+ $(,)?) => {
        <[()]>::len(&[$(count_variants!(@unit $variant)),+])
    };
    (@unit $variant:ident) => { () };
}

macro_rules! define_tsjs_bundles {
    ($($variant:ident => $file:expr),+ $(,)?) => {
        const TSJS_BUNDLE_COUNT: usize = count_variants!($($variant),+);

        #[repr(usize)]
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
        pub enum TsjsBundle {
            $( $variant ),+
        }

        const METAS: [TsjsMeta; TSJS_BUNDLE_COUNT] = [
            $(TsjsMeta::new($file, include_str!(concat!(env!("OUT_DIR"), "/", $file)))),+
        ];

        const ALL_BUNDLES: [TsjsBundle; TSJS_BUNDLE_COUNT] = [
            $(TsjsBundle::$variant),+
        ];

        impl TsjsBundle {
            pub const COUNT: usize = TSJS_BUNDLE_COUNT;

            pub const fn filename(self) -> &'static str {
                METAS[self as usize].filename
            }

            pub fn minified_filename(self) -> String {
                let base = self.filename();
                match base.strip_suffix(".js") {
                    Some(stem) => format!("{stem}.min.js"),
                    None => format!("{base}.min.js"),
                }
            }

            pub(crate) const fn bundle(self) -> &'static str {
                METAS[self as usize].bundle
            }

            pub(crate) fn filename_map(
            ) -> &'static ::std::collections::HashMap<&'static str, TsjsBundle> {
                static MAP: ::std::sync::OnceLock<
                    ::std::collections::HashMap<&'static str, TsjsBundle>,
                > = ::std::sync::OnceLock::new();

                MAP.get_or_init(|| {
                    ALL_BUNDLES
                        .iter()
                        .copied()
                        .map(|bundle| (bundle.filename(), bundle))
                        .collect::<::std::collections::HashMap<_, _>>()
                })
            }

            pub fn from_filename(name: &str) -> Option<Self> {
                Self::filename_map().get(name).copied()
            }
        }
    };
}

pub(crate) use count_variants;
pub(crate) use define_tsjs_bundles;
