use crate::{gpu, theme, AccountProvider};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Brand {
    Gmail,
    Naver,
    Sionic,
    Generic,
}

impl Brand {
    pub(crate) fn from_email(email: &str) -> Self {
        let email = email.trim();
        let Some((local, domain)) = email.split_once('@') else {
            return Self::Generic;
        };
        if local.is_empty() || email.chars().any(char::is_whitespace) || domain.contains('@') {
            return Self::Generic;
        }
        match domain.to_ascii_lowercase().as_str() {
            "gmail.com" | "googlemail.com" => Self::Gmail,
            "naver.com" => Self::Naver,
            "sionic.ai" => Self::Sionic,
            _ => Self::Generic,
        }
    }

    pub(crate) fn for_account(provider: AccountProvider, id: &str) -> Option<Self> {
        let email = match provider {
            AccountProvider::Claude => crate::settings::auth_probe(id).map(|p| p.email),
            AccountProvider::Codex => crate::settings::codex_identity(id),
        }?;
        (!email.trim().is_empty()).then(|| Self::from_email(&email))
    }

    pub(crate) fn width(self, size: f32) -> f32 {
        match self {
            Self::Naver => (size * 8.0 / 14.0) * 2315.0 / 444.0,
            _ => size,
        }
    }

    pub(crate) fn paint(self, g: &mut gpu::GpuRenderer, x: f32, y: f32, size: f32) {
        match self {
            Self::Gmail => g.queue_icon_colored("gmail", x, y, size, 1.0),
            Self::Sionic => g.queue_icon_colored("sionic", x, y, size, 1.0),
            Self::Naver => {
                // The official wordmark is wide; squeezing it into a square destroys its lettering.
                let height = size * 8.0 / 14.0;
                g.queue_icon_colored_rect(
                    "naver",
                    x,
                    y + (size - height) / 2.0,
                    self.width(size),
                    height,
                    1.0,
                );
            }
            Self::Generic => g.queue_icon("mail", x, y, size, theme::text_mute()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Brand;

    #[test]
    fn brands_come_from_exact_email_domains_not_slot_names() {
        for (email, brand) in [
            ("me@gmail.com", Brand::Gmail),
            ("me@googlemail.com", Brand::Gmail),
            ("sionic@gmail.com", Brand::Gmail),
            ("naver@gmail.com", Brand::Gmail),
            ("me@naver.com", Brand::Naver),
            ("me@sionic.ai", Brand::Sionic),
            (" ME@SIONIC.AI ", Brand::Sionic),
            ("me@example.com", Brand::Generic),
            ("me@sionic.ai.example.com", Brand::Generic),
            ("me@notnaver.com", Brand::Generic),
            ("me@gmail.com.example.com", Brand::Generic),
            ("me@team.sionic.ai", Brand::Generic),
            ("me@sionic.com", Brand::Generic),
            ("sionic", Brand::Generic),
            ("@gmail.com", Brand::Generic),
            ("me @gmail.com", Brand::Generic),
            ("me@ gmail.com", Brand::Generic),
            ("me@gmail.com@naver.com", Brand::Generic),
            ("", Brand::Generic),
        ] {
            assert_eq!(Brand::from_email(email), brand, "{email}");
        }
    }

    #[test]
    fn branding_reserves_its_real_width_at_every_ui_size() {
        for size in [12.0, 14.0, 28.0] {
            assert_eq!(Brand::Gmail.width(size), size);
            assert_eq!(Brand::Sionic.width(size), size);
            assert_eq!(Brand::Generic.width(size), size);
            assert!(
                (Brand::Naver.width(size) / (size * 8.0 / 14.0) - 2315.0 / 444.0).abs() < 0.001
            );
        }
    }
}
