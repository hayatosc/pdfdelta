//! Generated-input experiment for the pinned renderer's public observation API.
//! This executable accepts no external PDFs and never supplies source proofs.

use std::sync::{Arc, Mutex};

use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterCache,
    InterpreterSettings, Paint, PathDrawMode, SoftMask, TransformExt, font::Glyph, interpret_page,
};
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::kurbo::{Affine, BezPath, Rect, Shape};
use lopdf::{Document, Stream, dictionary};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SIZE: usize = 64;

#[derive(Default)]
struct Observer {
    events: Vec<Value>,
}

impl Observer {
    fn retain(&mut self, event: Value) {
        // Fixtures have fewer than 100 callbacks; accidental recursion must fail.
        assert!(self.events.len() < 1024);
        self.events.push(event);
    }
}

impl<'a> Device<'a> for Observer {
    fn set_soft_mask(&mut self, mask: Option<SoftMask<'a>>) {
        self.retain(json!({"kind":"soft_mask", "present":mask.is_some()}));
    }

    fn set_blend_mode(&mut self, mode: BlendMode) {
        self.retain(json!({"kind":"blend_mode", "mode":format!("{mode:?}")}));
    }

    fn draw_path(
        &mut self,
        path: &BezPath,
        transform: Affine,
        paint: &Paint<'a>,
        mode: &PathDrawMode,
    ) {
        let b = path.bounding_box();
        self.retain(json!({"kind":"path", "transform":transform.as_coeffs(),
            "local_bounds":[b.x0,b.y0,b.x1,b.y1], "mode":format!("{mode:?}"),
            "paint":format!("{paint:?}")}));
    }

    fn push_clip_path(&mut self, clip: &ClipPath) {
        self.retain(
            json!({"kind":"clip_push", "elements":clip.path.elements().len(),
            "fill":format!("{:?}",clip.fill)}),
        );
    }

    fn pop_clip_path(&mut self) {
        self.retain(json!({"kind":"clip_pop"}));
    }

    fn push_transparency_group(
        &mut self,
        opacity: f32,
        mask: Option<SoftMask<'a>>,
        mode: BlendMode,
    ) {
        self.retain(json!({"kind":"group_push", "opacity":opacity,
            "mask":mask.is_some(), "mode":format!("{mode:?}")}));
    }

    fn pop_transparency_group(&mut self) {
        self.retain(json!({"kind":"group_pop"}));
    }

    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        paint: &Paint<'a>,
        mode: &GlyphDrawMode,
    ) {
        let font_glyph = match glyph {
            Glyph::Outline(glyph) => Some(json!({
                "font_cache_key":format!("{:032x}",glyph.font_cache_key()),
                "glyph_id":format!("{:?}",glyph.glyph_id())
            })),
            Glyph::Type3(_) => None,
        };
        self.retain(
            json!({"kind":"glyph", "unicode":format!("{:?}",glyph.as_unicode()),
            "transform":transform.as_coeffs(), "glyph_transform":glyph_transform.as_coeffs(),
            "mode":format!("{mode:?}"), "paint":format!("{paint:?}"), "font_glyph":font_glyph}),
        );
    }

    fn draw_image(&mut self, image: Image<'a, '_>, transform: Affine) {
        self.retain(
            json!({"kind":"image", "width":image.width(), "height":image.height(),
            "transform":transform.as_coeffs()}),
        );
    }

    fn begin_marked_content(&mut self, tag: &[u8], mcid: Option<i32>) {
        self.retain(json!({"kind":"marked_begin", "tag_bytes":tag, "mcid":mcid}));
    }

    fn end_marked_content(&mut self) {
        self.retain(json!({"kind":"marked_end"}));
    }
}

fn fixture(content: &[u8]) -> Vec<u8> {
    assert!(content.len() < 4096);
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let font = document.add_object(dictionary! {
        "Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica"
    });
    let image = document.add_object(Stream::new(
        dictionary! {
            "Type"=>"XObject", "Subtype"=>"Image", "Width"=>1, "Height"=>1,
            "ColorSpace"=>"DeviceRGB", "BitsPerComponent"=>8
        },
        vec![255, 0, 0],
    ));
    let form = document.add_object(Stream::new(
        dictionary! {
        "Type"=>"XObject", "Subtype"=>"Form", "BBox"=>vec![0.into(),0.into(),64.into(),64.into()],
            "Resources"=>dictionary!{"Font"=>dictionary!{"F1"=>font}},
            "Group"=>dictionary!{"S"=>"Transparency", "CS"=>"DeviceRGB", "I"=>true}
        },
        b"/P <</MCID 7>> BDC BT /F1 8 Tf 10 40 Td (A) Tj ET EMC".to_vec(),
    ));
    let state = document.add_object(dictionary! {
        "Type"=>"ExtGState", "ca"=>0.5_f32, "CA"=>0.5_f32, "BM"=>"Multiply"
    });
    let stream = document.add_object(Stream::new(dictionary! {}, content.to_vec()));
    let page = document.add_object(dictionary! {
        "Type"=>"Page", "Parent"=>pages, "MediaBox"=>vec![0.into(),0.into(),64.into(),64.into()],
        "Resources"=>dictionary!{
            "Font"=>dictionary!{"F1"=>font},
            "XObject"=>dictionary!{"Im"=>image,"Fm"=>form},
            "ExtGState"=>dictionary!{"Gs"=>state}
        }, "Contents"=>stream
    });
    document.objects.insert(
        pages,
        dictionary! {
            "Type"=>"Pages", "Kids"=>vec![page.into()], "Count"=>1
        }
        .into(),
    );
    let catalog = document.add_object(dictionary! {"Type"=>"Catalog", "Pages"=>pages});
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("serialize bounded fixture");
    assert!(bytes.len() < 16384);
    bytes
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn render(pdf: &Pdf) -> (Vec<u8>, Vec<String>) {
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&warnings);
    let settings = InterpreterSettings {
        warning_sink: Arc::new(move |warning| {
            sink.lock()
                .expect("warning sink lock")
                .push(format!("{warning:?}"));
        }),
        ..Default::default()
    };
    let pixmap = hayro::render(
        &pdf.pages()[0],
        &hayro::RenderCache::new(),
        &settings,
        &hayro::RenderSettings {
            width: Some(SIZE as u16),
            height: Some(SIZE as u16),
            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
            ..Default::default()
        },
    );
    let warnings = warnings.lock().expect("warning sink lock").clone();
    (pixmap.data_as_u8_slice().to_vec(), warnings)
}

fn observe(pdf: &Pdf) -> (Observer, Vec<String>) {
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&warnings);
    let page = &pdf.pages()[0];
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, SIZE as f64, SIZE as f64),
        &cache,
        page.xref(),
        InterpreterSettings {
            warning_sink: Arc::new(move |warning| {
                sink.lock()
                    .expect("warning sink lock")
                    .push(format!("{warning:?}"));
            }),
            ..Default::default()
        },
    );
    let mut observer = Observer::default();
    interpret_page(page, &mut context, &mut observer);
    let warnings = warnings.lock().expect("warning sink lock").clone();
    (observer, warnings)
}

fn coarse(bytes: &[u8]) -> Vec<u8> {
    (4..SIZE)
        .step_by(8)
        .flat_map(|y| {
            (4..SIZE).step_by(8).flat_map(move |x| {
                bytes[(y * SIZE + x) * 4..(y * SIZE + x) * 4 + 3]
                    .iter()
                    .copied()
            })
        })
        .collect()
}

fn local_difference(old: &[u8], new: &[u8]) -> Value {
    let mut count = 0;
    let mut bounds = [SIZE, SIZE, 0, 0];
    for (index, (a, b)) in old
        .as_chunks::<4>()
        .0
        .iter()
        .zip(new.as_chunks::<4>().0)
        .enumerate()
    {
        if a != b {
            count += 1;
            let (x, y) = (index % SIZE, index / SIZE);
            bounds = [
                bounds[0].min(x),
                bounds[1].min(y),
                bounds[2].max(x + 1),
                bounds[3].max(y + 1),
            ];
        }
    }
    json!({"changed_pixels":count,"exclusive_pixel_bounds":(count>0).then_some(bounds),
        "pixel_comparisons":SIZE*SIZE,"source_identity":false})
}

fn main() {
    let bytes = fixture(
        b"q 0 0 64 64 re W n /Gs gs 5 5 7 7 re f Q q 6 0 0 6 40 40 cm /Im Do Q /Fm Do /Fm Do",
    );
    let pdf = Pdf::new(bytes.clone()).expect("parse generated fixture");
    let (before, render_warnings) = render(&pdf);
    let (observer, observer_warnings) = observe(&pdf);
    let (after, after_warnings) = render(&pdf);
    assert_eq!(before, after);
    assert_eq!(render_warnings, after_warnings);
    let glyphs: Vec<_> = observer
        .events
        .iter()
        .filter(|event| event["kind"] == "glyph")
        .collect();
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0], glyphs[1]);
    for kind in [
        "path",
        "glyph",
        "image",
        "clip_push",
        "group_push",
        "marked_begin",
        "blend_mode",
    ] {
        assert!(
            observer.events.iter().any(|event| event["kind"] == kind),
            "missing {kind}"
        );
    }
    let baseline_bytes = fixture(b"");
    let baseline = Pdf::new(baseline_bytes.clone()).expect("parse blank fixture");
    let (blank, blank_warnings) = render(&baseline);
    let mut tiny = Vec::new();
    for (name, content) in [
        ("decimal_point", b"22 22 0.8 0.8 re f".as_slice()),
        ("minus_sign", b"0.3 w 22 23 m 26 23 l S".as_slice()),
        ("subscript", b"BT /F1 3 Tf 22 22 Td (2) Tj ET".as_slice()),
        ("thin_line", b"0.1 w 23 22 m 23 26 l S".as_slice()),
    ] {
        let bytes = fixture(content);
        let pdf = Pdf::new(bytes.clone()).expect("parse generated fixture");
        let (pixels, warnings) = render(&pdf);
        let exact = local_difference(&blank, &pixels);
        assert!(exact["changed_pixels"].as_u64().expect("pixel count") > 0);
        assert_eq!(coarse(&blank), coarse(&pixels), "coarse fixture {name}");
        tiny.push(
            json!({"name":name,"pdf_sha256":digest(&bytes),"rgba_sha256":digest(&pixels),
            "coarse_equal":true,"coarse_sample_count":64,"full_grid":exact,"warnings":warnings}),
        );
    }
    println!("{}",serde_json::to_string_pretty(&json!({
        "version":1,"renderer":"hayro-0.7.1","interpreter":"hayro-interpret-0.7.0",
        "profile":{"width":SIZE,"height":SIZE,"dpi":72,"background":"white",
            "annotations":true,"optional_content":"interpreter default catalog state"},
        "observer_pdf_sha256":digest(&bytes),"render_rgba_sha256":digest(&before),
        "before_after_side_pass_pixels_equal":true,"before_after_render_warnings_equal":true,
        "pass_through_renderer_hook_tested":false,
        "limitation":"Public render does not accept a Device and its Renderer is private. Equality covers rendering before and after an independent observation pass, not an attached observer.",
        "form_invocations":2,"indistinguishable_glyph_callback_payloads":true,
        "native_source_bindings":0,"render_warnings":render_warnings,"observer_warnings":observer_warnings,
        "events":observer.events,"blank_pdf_sha256":digest(&baseline_bytes),"blank_warnings":blank_warnings,
        "tiny_feature_cases":tiny,
        "adoption":"Keep this as a bounded generated-input experiment. Do not connect observer callbacks or coarse-feature equality to strict source evidence."
    })).expect("serialize experiment report"));
}

#[test]
fn generated_observer_and_tiny_feature_counterexamples() {
    main();
}
