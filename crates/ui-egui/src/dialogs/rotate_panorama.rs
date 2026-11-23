//! Image › Rotate Panorama… (#15726): turns a latlong (2:1 equirectangular) image on its sphere by Yaw, Pitch and Roll,
//! to level a tilted horizon or set which way is the front. OK runs `image.rotatePanorama` on every pixel layer and
//! mask, as one History step. In View › 360° View the view previews the turn while the dialog is open and draws the
//! true horizon and the front direction, so the image's horizon can be lined up with them.

use craft::ui::contract::{Bind, Show as _};
use craft::ui::dialog::{DialogId, DialogResult, DialogStack, StackDialog, dialog_id};
use craft::ui::display::Text;
use craft::ui::patterns::form_dialogs::FormDialog;
use craft::ui::runtime::env::Label;
use craft::ui::text::NumberField;
use photocraft_engine::equirect::Rot;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;

/// The command OK runs, and the menu item's id.
pub(crate) const COMMAND: &str = "image.rotatePanorama";

/// The turn, in degrees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Turn {
    yaw: Option<f64>,
    pitch: Option<f64>,
    roll: Option<f64>,
}

impl Turn {
    fn degrees(&self) -> [f64; 3] {
        [self.yaw, self.pitch, self.roll].map(|v| v.filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-360.0, 360.0))
    }

    fn params(&self) -> Value {
        let [yaw, pitch, roll] = self.degrees();
        json!({"yaw": yaw, "pitch": pitch, "roll": roll})
    }
}

/// The Rotate Panorama dialog.
pub(crate) struct RotatePanoramaDialog {
    turn: Turn,
}

impl StackDialog for RotatePanoramaDialog {
    type Result = Turn;

    fn id(&self) -> DialogId {
        dialog_id("photocraft.rotatePanorama")
    }

    fn show(&mut self, ui: &mut egui::Ui, _stack: &DialogStack) -> Option<DialogResult<Turn>> {
        let shown = FormDialog::new(Label::from(tl!("Rotate Panorama").to_string()), Bind::bound(&mut self.turn)).show(ui, |ui, _form, turn| {
            Text::new(Label::from(tl!("Yaw turns the image right, Pitch tilts it up and Roll turns it clockwise, in degrees.").to_string())).show(ui);
            for (name, value) in [(tl!("Yaw"), &mut turn.yaw), (tl!("Pitch"), &mut turn.pitch), (tl!("Roll"), &mut turn.roll)] {
                ui.horizontal(|ui| {
                    Text::new(Label::from(name.to_string())).show(ui);
                    NumberField::new(Label::from(name.to_string()), Bind::bound(value)).range(-360.0..=360.0).show(ui);
                });
            }
            true
        });
        match (shown.accepted, shown.closed) {
            (Some(_), _) => Some(DialogResult::Ok(self.turn)),
            (None, Some(_)) => Some(DialogResult::Cancel),
            (None, None) => None,
        }
    }

    fn confirm(&mut self, _ctx: &egui::Context) -> Option<Turn> {
        Some(self.turn)
    }

    fn json_fields(&self) -> Option<Value> {
        let [yaw, pitch, roll] = self.turn.degrees();
        Some(json!({"__command": COMMAND, "yaw": yaw, "pitch": pitch, "roll": roll}))
    }

    fn set_json_field(&mut self, ctx: &egui::Context, field: &str, value: Value) -> Result<(), String> {
        let v =
            value.as_f64().filter(|v| v.is_finite() && v.abs() <= 360.0).ok_or_else(|| format!("field `{field}` takes degrees, -360..=360, got {value}"))?;
        match field {
            "yaw" => self.turn.yaw = Some(v),
            "pitch" => self.turn.pitch = Some(v),
            "roll" => self.turn.roll = Some(v),
            _ => return Err(format!("the Rotate Panorama dialog has no field `{field}`")),
        }
        ctx.request_repaint();
        Ok(())
    }
}

/// Opens the dialog for the active document.
pub(crate) fn open(app: &mut PhotocraftApp) -> Result<Value, String> {
    app.session.active().ok_or_else(|| tl!("No document is open.").to_string())?;
    app.framework.dialogs.open(RotatePanoramaDialog { turn: Turn::default() });
    Ok(json!({"dialog": "photocraft.rotatePanorama"}))
}

/// The dialog's result pump (a `TAKERS` row): OK applies the turn.
pub(crate) fn take(app: &mut PhotocraftApp, _ctx: &egui::Context) {
    let Some(result) = app.framework.dialogs.take::<RotatePanoramaDialog>() else { return };
    if let DialogResult::Ok(turn) = result
        && turn.degrees() != [0.0; 3]
        && let Err(e) = app.run(COMMAND, turn.params())
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// While the dialog is open, the rotation it would apply, for the 360° View's preview.
pub(crate) fn preview(app: &PhotocraftApp) -> Option<Rot> {
    let [yaw, pitch, roll] = app.framework.dialogs.get::<RotatePanoramaDialog>()?.turn.degrees();
    Some(photocraft_engine::projected_transform::panorama_rotation(yaw, pitch, roll))
}
