//! Frames of the approved Aster board. The original pixels preserve the character's eyes and painterly finish.
use dioxus::prelude::*;

/// A character illustration already present on the approved board, never a separately redrawn variant.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AsterPose {
    Standing,
    Explore,
    Illustrate,
    Guide,
}

impl AsterPose {
    fn frame(self) -> (&'static str, &'static str, u16, u16) {
        match self {
            Self::Standing => ("standing", "0 20 560 815", 560, 815),
            Self::Explore => ("explore", "570 205 285 285", 285, 285),
            Self::Illustrate => ("illustrate", "815 232 295 265", 295, 265),
            Self::Guide => ("guide", "1106 194 244 296", 244, 296),
        }
    }
}

/// An SVG viewport onto a single, unchanged canonical board supplied through the public asset resolver.
/// Decorative: nearby copy names Aster, and every placement is hidden from assistive technology.
#[component]
pub fn AsterArtwork(src: String, pose: AsterPose, #[props(default)] class: String) -> Element {
    let (name, frame, width, height) = pose.frame();
    rsx! {
        svg {
            class: "aster-artwork {class}",
            "viewBox": frame,
            width,
            height,
            "data-aster-pose": name,
            "aria-hidden": "true",
            "focusable": "false",
            image { href: src, width: "1774", height: "887", preserve_aspect_ratio: "xMinYMin meet" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pose_frames_original_pixels_without_redrawing_the_face() {
        fn app() -> Element {
            rsx! {
                for pose in [AsterPose::Standing, AsterPose::Explore, AsterPose::Illustrate, AsterPose::Guide] {
                    AsterArtwork { src: "/public/img/canonical-board.png", pose }
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert_eq!(
            html.matches("href=\"/public/img/canonical-board.png\"")
                .count(),
            4
        );
        assert_eq!(html.matches("aria-hidden=\"true\"").count(), 4);
        for frame in [
            "0 20 560 815",
            "570 205 285 285",
            "815 232 295 265",
            "1106 194 244 296",
        ] {
            assert!(html.contains(&format!("viewBox=\"{frame}\"")), "{frame}");
            let bounds: Vec<u32> = frame
                .split_whitespace()
                .map(|v| v.parse().unwrap())
                .collect();
            assert!(bounds[0] + bounds[2] <= 1774 && bounds[1] + bounds[3] <= 887);
        }
        assert!(!html.contains("<path") && !html.contains("<ellipse"));
    }
}
