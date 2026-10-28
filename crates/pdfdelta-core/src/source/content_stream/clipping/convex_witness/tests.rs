use super::*;
use crate::source::content_stream::clipping::{ClipRegion, Quad};

#[derive(serde::Deserialize)]
struct Case {
    glyph_bits: [u64; 4],
    quad_bits: Vec<[[u64; 2]; 4]>,
    expected: String,
    tag: String,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!("oracle.json")).expect("independent rational fixtures")
}

fn geometry(case: &Case) -> (Rect, ClipRegion) {
    let [x0, y0, x1, y1] = case.glyph_bits.map(f64::from_bits);
    let glyph = Rect {
        min: Vec2 { x: x0, y: y0 },
        max: Vec2 { x: x1, y: y1 },
    };
    let mut region = ClipRegion::Unbounded;
    for quad in &case.quad_bits {
        let quad = Quad::new(quad.map(|[x, y]| Vec2 {
            x: f64::from_bits(x),
            y: f64::from_bits(y),
        }))
        .expect("strict convex fixture");
        region = region.intersect_quad(quad);
    }
    (glyph, region)
}

fn ddpm() -> (Rect, ConvexClip) {
    let case = cases().remove(0);
    let (glyph, ClipRegion::Convex(clip)) = geometry(&case) else {
        panic!("convex DDPM fixture")
    };
    (glyph, (*clip).clone())
}

#[test]
fn rational_oracle_rejects_every_false_classification() {
    let corpus = cases();
    assert_eq!(corpus.len(), 578);
    let mut gained = 0;
    let mut certified = 0;
    let mut held = 0;
    for case in corpus {
        let (glyph, region) = geometry(&case);
        let before = region.glyph_status(glyph);
        let mut used = 0;
        let after = if let ClipRegion::Convex(clip) = &region {
            let mut budget = Budget {
                used: 0,
                limit: usize::MAX,
                exhausted: false,
            };
            if nominate(clip, glyph, &mut budget)
                .is_some_and(|point| certify(clip, glyph, point, &mut budget))
            {
                assert_eq!(case.expected, "PartiallyOutside", "{}", case.tag);
                certified += 1;
            }
            clip.glyph_status_with_witness(glyph, &mut used, 1_000_000)
        } else {
            region.glyph_status(glyph)
        };
        match (&before, &after) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a, b, "{}", case.tag);
                assert_eq!(used, 0, "known verdict pays no fallback");
            }
            (Ok(_), Err(_)) => panic!("lost existing verdict: {}", case.tag),
            (Err(_), Ok(_)) => gained += 1,
            (Err(_), Err(_)) => held += 1,
        }
        if let Ok(status) = after {
            assert_eq!(format!("{status:?}"), case.expected, "{}", case.tag);
        }
    }
    assert!(gained >= 2, "both recorded DDPM cases must gain a verdict");
    eprintln!(
        "C27_ORACLE cases=578 new_verdicts={gained} certified_partial={certified} held={held}"
    );
}

#[test]
fn recorded_ddpm_overlap_requires_an_interior_witness() {
    let (glyph, clip) = ddpm();
    assert!(matches!(
        clip.glyph_status(glyph),
        Err(Error::Unresolved(_))
    ));
    let mut used = 0;
    assert_eq!(
        clip.glyph_status_with_witness(glyph, &mut used, 1_000_000)
            .expect("certified overlap"),
        GlyphPathClipStatus::PartiallyOutside
    );
    assert!(used > 0);
    eprintln!("C27_DDPM fallback_work={used}");
}

#[test]
fn every_budget_cut_preserves_uncertainty_and_paid_work() {
    let (glyph, clip) = ddpm();
    let initial = 7;
    let mut total = initial;
    clip.glyph_status_with_witness(glyph, &mut total, usize::MAX)
        .expect("complete certificate");
    for limit in initial..total {
        let mut used = initial;
        assert!(matches!(
            clip.glyph_status_with_witness(glyph, &mut used, limit),
            Err(Error::LimitExceeded {
                resource: "convex clipping work",
                ..
            })
        ));
        assert!((initial..=limit).contains(&used));
    }
    let mut used = usize::MAX;
    assert!(matches!(
        clip.glyph_status_with_witness(glyph, &mut used, usize::MAX),
        Err(Error::LimitExceeded { .. })
    ));
    assert_eq!(used, usize::MAX);
}

#[test]
fn untrusted_point_does_not_certify_visibility_or_absence() {
    let (glyph, clip) = ddpm();
    for point in corners(glyph).into_iter().chain([
        Vec2 {
            x: f64::NAN,
            y: 0.0,
        },
        Vec2 { x: 0.0, y: 0.0 },
    ]) {
        let mut budget = Budget {
            used: 0,
            limit: usize::MAX,
            exhausted: false,
        };
        assert!(!certify(&clip, glyph, point, &mut budget));
    }
    let enclosing = ConvexClip {
        bounds: glyph,
        quads: vec![Quad::rectangle(glyph)],
    };
    let point = Vec2 {
        x: glyph.min.x.midpoint(glyph.max.x),
        y: glyph.min.y.midpoint(glyph.max.y),
    };
    let mut budget = Budget {
        used: 0,
        limit: usize::MAX,
        exhausted: false,
    };
    assert!(
        !certify(&enclosing, glyph, point, &mut budget),
        "no exterior witness"
    );
}

#[test]
fn unsupported_size_and_arithmetic_do_not_create_a_certificate() {
    let (glyph, mut clip) = ddpm();
    clip.quads.resize(MAX_QUADS + 1, clip.quads[0]);
    let mut used = 0;
    assert!(matches!(
        clip.glyph_status_with_witness(glyph, &mut used, 1_000_000),
        Err(Error::Unresolved(_))
    ));
    assert_eq!(used, 8);
    for bad in [
        Rect {
            min: glyph.min,
            max: glyph.min,
        },
        Rect {
            min: Vec2 {
                x: f64::NAN,
                y: 0.0,
            },
            max: glyph.max,
        },
        Rect {
            min: Vec2 {
                x: -1.0e308,
                y: -1.0e308,
            },
            max: Vec2 {
                x: 1.0e308,
                y: 1.0e308,
            },
        },
    ] {
        let (_, clip) = ddpm();
        let mut budget = Budget {
            used: 0,
            limit: usize::MAX,
            exhausted: false,
        };
        assert!(nominate(&clip, bad, &mut budget).is_none());
    }
}

#[test]
fn bounded_buffer_refuses_overflow_and_nonfinite_vertices() {
    let mut buffer = [Vec2 { x: 0.0, y: 0.0 }; MAX_VERTICES];
    let mut len = MAX_VERTICES;
    assert!(push(&mut buffer, &mut len, Vec2 { x: 0.0, y: 0.0 }).is_none());
    assert_eq!(len, MAX_VERTICES);
    len = 0;
    assert!(
        push(
            &mut buffer,
            &mut len,
            Vec2 {
                x: f64::INFINITY,
                y: 0.0
            }
        )
        .is_none()
    );
    assert_eq!(len, 0);
}
