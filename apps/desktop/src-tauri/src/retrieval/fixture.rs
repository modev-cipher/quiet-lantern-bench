//! Preserved B-0 spike fixture: a deterministic ~2k-segment English/Arabic/
//! code-switched book (xorshift PRNG, fixed seed) so regression runs are
//! comparable to the proven spike numbers. Ported unchanged from the spike
//! crate, plus typed provenance: a deterministic minority of rows (and one
//! planted generated needle) are classed `Generated` to drive the T9
//! generated-is-not-evidence fixtures.

use super::provider::{Provenance, SegmentRow};

pub struct Xorshift(u64);
impl Xorshift {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next_u64() % items.len() as u64) as usize]
    }
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo)
    }
}

const TOPICS: &[&str] = &[
    "the astrolabe",
    "dead reckoning",
    "the magnetic compass",
    "celestial navigation",
    "the marine chronometer",
    "latitude sailing",
    "the sextant",
    "portolan charts",
    "the lunar distance method",
    "monsoon wind patterns",
    "the kamal",
    "star catalogs",
    "the cross-staff",
    "longitude determination",
    "tidal tables",
    "the quadrant",
    "polaris observations",
    "the equation of time",
    "compass declination",
    "great circle routes",
];
const AGENTS: &[&str] = &[
    "Phoenician traders",
    "Polynesian wayfinders",
    "Abbasid astronomers",
    "Portuguese pilots",
    "Dutch cartographers",
    "Chinese admirals",
    "Greek geometers",
    "Venetian merchants",
    "Ottoman navigators",
    "British hydrographers",
    "Arab dhow captains",
    "Norse seafarers",
];
const VERBS: &[&str] = &[
    "refined",
    "documented",
    "rediscovered",
    "systematized",
    "transmitted",
    "challenged",
    "measured",
    "standardized",
    "misunderstood",
    "perfected",
    "taught",
    "guarded",
];
const OBJECTS: &[&str] = &[
    "star altitudes near the horizon",
    "the error of their instruments",
    "seasonal current reversals",
    "the geometry of the celestial sphere",
    "tables of solar declination",
    "the drift of the magnetic pole",
    "sight-reduction procedures",
    "the calibration of sandglasses",
    "coastal landmark sequences",
    "the arithmetic of running fixes",
];
const CLAUSES: &[&str] = &[
    "long before printed almanacs reached working sailors",
    "though court astronomers dismissed the practice as folklore",
    "and the technique spread along the pepper routes within a generation",
    "which halved the reckoning error on the outbound passage",
    "a fact rediscovered only when the logbooks were translated",
    "despite the guild's attempts to keep the method secret",
    "and shipwreck rates on the cape route fell measurably",
    "as the surviving rutters make unmistakably clear",
];

// Arabic sentence bank (golden-age astronomy register). Includes the
// morphological families the stemmer probes rely on: كتاب/الكتاب/كتب/يكتبون,
// نجم/النجوم, فلك/الأفلاك, بحر/البحار, قياس/يقيسون.
const AR_SENTENCES: &[&str] = &[
    "وصف المؤلف في هذا الكتاب طريقة قياس ارتفاع النجوم فوق الأفق باستخدام الأسطرلاب",
    "كان البحارة العرب يقيسون ارتفاع النجم القطبي بأصابع اليد الممدودة نحو الأفق",
    "ذكر ابن ماجد في أرجوزته أن معرفة المنازل القمرية أصل من أصول علم البحر",
    "جمع الفلكيون في بغداد جداول دقيقة لمواقع الكواكب واستعملها الملاحون قروناً طويلة",
    "كتب العلماء رسائل كثيرة في تصحيح سمت القبلة بواسطة حساب المثلثات الكروية",
    "اعتمد الربابنة على هبوب الرياح الموسمية في عبور المحيط الهندي ذهاباً وإياباً",
    "الأفلاك عند بطليموس دوائر متداخلة تدور حولها الكواكب دوراناً منتظماً",
    "يكتبون أسماء المنازل الثمانية والعشرين على حاشية الخريطة تذكرة للربان",
    "في البحار الجنوبية تختفي النجوم الشمالية ويظهر للناظر سهيل وما جاوره",
    "صنع الصفّار في طليطلة أسطرلاباً نحاسياً بقي مرجعاً في القياس مئات السنين",
    "المكتبة التي أنشأها المأمون حفظت كتب اليونان في الفلك وترجمتها إلى العربية",
    "قاس البيروني محيط الأرض بطريقة الانخفاض الأفقي فأصاب إصابة عجيبة",
];

// Code-switched bank: English prose citing Arabic terms inline.
const CS_SENTENCES: &[&str] = &[
    "The instrument the pilots called الكمال was nothing more than a knotted cord and a wooden tablet, yet it fixed latitude reliably.",
    "Ibn Majid's treatise, the كتاب الفوائد, catalogs twenty-eight lunar mansions used for open-water steering.",
    "What Greek texts called klimata, the Baghdad school rendered as الأقاليم, seven latitude bands girdling the inhabited world.",
    "The navigator's reckoning, or حساب, blended memorized star courses with running estimates of speed and drift.",
    "European sailors borrowed the term قياس for the altitude measurement itself, corrupting it in Portuguese logbooks.",
    "The عالم الفلك of the observatory and the معلم of the dhow rarely met, though they measured the same stars.",
    "A مجرى, one day's sail before the monsoon, became the standard distance unit across the Indian Ocean trade.",
    "The star Canopus, سهيل to the Arab pilots, marked the southern gateway routes toward Zanzibar.",
];

/// Planted needle segments with globally unique terms, for recall sanity pins.
pub const NEEDLES: &[(&str, &str)] = &[
    ("zerbadar", "The obscure Gujarati pilot Zerbadar recorded a triple lunar occultation off Calicut in 1503, an event no European almanac predicted."),
    ("qandarun", "Workshop inventories mention a brass alloy called qandarun, prized because it held engraved altitude scales without corroding in salt air."),
];

/// Planted GENERATED needle: retrievable, but never quotable evidence (T9).
pub const GENERATED_NEEDLE: (&str, &str) = (
    "velgorath",
    "In this generated overview, the invented scholar Velgorath summarizes the chapter's themes for listeners.",
);

pub fn generate(seed: u64) -> Vec<SegmentRow> {
    let mut rng = Xorshift::new(seed);
    let mut rows = Vec::with_capacity(2100);
    let mut idx: u32 = 0;
    let mut page: u32 = 1;
    let mut seg_on_page = 0u32;

    // 12 chapters. Chapter 9 = Arabic (translated sources); chapters 7-8 lean
    // code-switched; the rest English with occasional code-switched rows.
    for chapter in 1..=12u32 {
        let seg_count = rng.range(150, 190);
        for _ in 0..seg_count {
            let text = if chapter == 9 {
                let n = rng.range(2, 4);
                (0..n)
                    .map(|_| *rng.pick(AR_SENTENCES))
                    .collect::<Vec<_>>()
                    .join(". ")
            } else {
                let roll = rng.next_u64() % 100;
                let cs_bias = if (7..=8).contains(&chapter) { 30 } else { 6 };
                if roll < cs_bias {
                    format!("{} {}", rng.pick(CS_SENTENCES), english_sentence(&mut rng))
                } else {
                    format!(
                        "{} {}",
                        english_sentence(&mut rng),
                        english_sentence(&mut rng)
                    )
                }
            };
            // Every 50th row models generated Teacher prose (chapter intros,
            // figure explanations): indexed for recall, refused as evidence.
            let provenance = if idx % 50 == 0 {
                Provenance::Generated
            } else {
                Provenance::SourceFaithful
            };
            rows.push(SegmentRow {
                chapter,
                page,
                idx,
                text,
                provenance,
            });
            idx += 1;
            seg_on_page += 1;
            if seg_on_page >= 4 {
                page += 1;
                seg_on_page = 0;
            }
        }
    }
    // Plant needles at deterministic-but-arbitrary positions.
    let n_rows = rows.len();
    for (i, (_, text)) in NEEDLES.iter().enumerate() {
        let pos = (i + 1) * n_rows / (NEEDLES.len() + 1);
        rows[pos].text = format!("{} {}", text, rows[pos].text);
        rows[pos].provenance = Provenance::SourceFaithful;
    }
    // Plant the generated needle on a Generated row.
    let gpos = n_rows / 3 + 1;
    let gpos = gpos - (gpos % 50); // snap to a Generated row
    rows[gpos].text = format!("{} {}", GENERATED_NEEDLE.1, rows[gpos].text);
    rows
}

fn english_sentence(rng: &mut Xorshift) -> String {
    format!(
        "{} {} {} in the study of {}, {}.",
        rng.pick(AGENTS),
        rng.pick(VERBS),
        rng.pick(OBJECTS),
        rng.pick(TOPICS),
        rng.pick(CLAUSES)
    )
}
