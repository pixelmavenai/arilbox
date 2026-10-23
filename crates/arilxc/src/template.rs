//! Allowed templates and their images on images.linuxcontainers.org (`SPEC.md` §5.5).

use arilbox_core::config::TemplateRef;

/// An image for `lxc-create -t download`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    /// `--dist`
    pub dist: &'static str,
    /// `--release` (the image server uses codenames for some distros).
    pub release: &'static str,
}

/// `(distro, release)` as used in the API → image. Anything not listed is rejected.
const TEMPLATES: &[(&str, &str, Image)] = &[
    (
        "debian",
        "13",
        Image {
            dist: "debian",
            release: "trixie",
        },
    ),
    (
        "ubuntu",
        "24.04",
        Image {
            dist: "ubuntu",
            release: "noble",
        },
    ),
    (
        "almalinux",
        "10",
        Image {
            dist: "almalinux",
            release: "10",
        },
    ),
    (
        "alpine",
        "edge",
        Image {
            dist: "alpine",
            release: "edge",
        },
    ),
];

/// The image for an allowed template, or `None`.
pub fn image_for(template: &TemplateRef) -> Option<Image> {
    TEMPLATES
        .iter()
        .find(|(d, r, _)| *d == template.distro && *r == template.release)
        .map(|(_, _, image)| *image)
}

/// All allowed templates, for listing in the API/UI.
pub fn allowed_templates() -> impl Iterator<Item = TemplateRef> {
    TEMPLATES.iter().map(|(d, r, _)| TemplateRef {
        distro: (*d).into(),
        release: (*r).into(),
    })
}

/// LXC architecture name of this host, or `None` if unsupported.
pub fn host_arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(d: &str, r: &str) -> TemplateRef {
        TemplateRef {
            distro: d.into(),
            release: r.into(),
        }
    }

    #[test]
    fn spec_templates_map_to_images() {
        assert_eq!(image_for(&t("debian", "13")).unwrap().release, "trixie");
        assert_eq!(image_for(&t("ubuntu", "24.04")).unwrap().release, "noble");
        assert!(image_for(&t("almalinux", "10")).is_some());
        assert!(image_for(&t("alpine", "edge")).is_some());
        assert_eq!(allowed_templates().count(), 4);
    }

    #[test]
    fn others_are_rejected() {
        assert!(image_for(&t("debian", "trixie")).is_none());
        assert!(image_for(&t("debian", "12")).is_none());
        assert!(image_for(&t("arch", "current")).is_none());
    }
}
