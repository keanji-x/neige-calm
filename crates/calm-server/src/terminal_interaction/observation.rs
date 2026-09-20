//! Presentation is independent of capture identity and input authority.
use anyhow::Result;
use calm_terminal_view::{Frame, Rasterizer};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::OnceCell;

/// A wall-clock fact of an observation as milliseconds since the Unix epoch. A time before
/// the epoch is 0, not a panic and not a negative number.
pub(super) fn epoch_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ObservationFormat {
    #[default]
    Text,
    Image,
}

impl ObservationFormat {
    pub(super) async fn render_image(
        self,
        raster: &OnceCell<Arc<Rasterizer>>,
        frame: &Frame,
    ) -> Result<Option<Vec<u8>>> {
        // Ordinary terminal operation needs the captured text and authority, not
        // fonts or PNG rendering. Explicit image failures remain errors.
        if self == Self::Text {
            return Ok(None);
        }
        let raster = raster
            .get_or_try_init(|| async {
                tokio::task::spawn_blocking(Rasterizer::system)
                    .await?
                    .map(Arc::new)
            })
            .await?
            .clone();
        let image_frame = frame.clone();
        Ok(Some(
            tokio::task::spawn_blocking(move || raster.png(&image_frame)).await??,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_terminal_view::TerminalView;

    #[test]
    fn epoch_ms_is_milliseconds_since_the_epoch_and_never_negative() {
        assert_eq!(epoch_ms(UNIX_EPOCH), 0);
        assert_eq!(
            epoch_ms(UNIX_EPOCH + std::time::Duration::from_millis(1_780_977_421_069)),
            1_780_977_421_069
        );
        assert_eq!(
            epoch_ms(UNIX_EPOCH - std::time::Duration::from_secs(1)),
            0,
            "before the epoch: 0, never a panic"
        );
        let now = epoch_ms(SystemTime::now());
        assert!(now > 1_700_000_000_000, "{now}");
    }

    #[tokio::test]
    async fn text_observation_does_not_initialize_rasterizer() {
        let raster = OnceCell::new();
        let frame = TerminalView::new(80, 24, [220; 3], [20; 3])
            .unwrap()
            .frame(0)
            .unwrap();
        let png = ObservationFormat::default()
            .render_image(&raster, &frame)
            .await
            .unwrap();
        assert!(raster.get().is_none(), "text must never load system fonts");
        assert!(png.is_none(), "text must never render an image");
    }

    #[tokio::test]
    async fn image_render_failure_is_explicit_and_does_not_disable_text() {
        let raster = OnceCell::new();
        let mut frame = TerminalView::new(80, 24, [220; 3], [20; 3])
            .unwrap()
            .frame(0)
            .unwrap();
        // Zero image geometry cannot be rasterized. The text branch must not
        // consult the rasterizer, either before or after an image error.
        frame.cols = 0;
        frame.rows = 0;
        frame.cells.clear();
        assert!(
            ObservationFormat::Text
                .render_image(&raster, &frame)
                .await
                .unwrap()
                .is_none()
        );
        assert!(raster.get().is_none());
        assert!(
            ObservationFormat::Image
                .render_image(&raster, &frame)
                .await
                .is_err()
        );
        assert!(
            ObservationFormat::Text
                .render_image(&raster, &frame)
                .await
                .unwrap()
                .is_none()
        );
    }
}
