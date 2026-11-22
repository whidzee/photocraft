use crate::cmd::{CommandSpec, has_paintable};

/// Painting: the stroke commands (the flat canvas and View › 360° View).
pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "paint.stroke",
            "Brush Stroke",
            [],
            None,
            r##"{"points":[[x,y,pressure?,tiltX?,tiltY?,rotation?,timeMs?,wheel?],…],"brush":{…BrushSettings}?,"preset":name?,"size":0.5..5000 px?,"hardness":0..1?,"opacity":0..1?,"flow":0..1?,"fill":bool=false (fill the region the closed `points` enclose with the brush colour, its edge anti-aliased and softened by hardness, instead of stamping the outline; a closed gesture paints a filled dab, not a ring),"spacing":0..10?,"color":"#rrggbb"?=foreground,"mode":"normal|multiply|screen|…|clear (erases with this brush)"="normal","erase":bool?,"smoothing":0..1?,"zoom":number=1,"seed":u64?,"target":"pixels"|"mask"|"filterMask"|"quickMask"|{"channel":i}=Channels panel target}"##,
            has_paintable,
            crate::brush_cmds::paint_stroke
        ),
        cmd!(
            "paint.projectedStroke",
            "Projected Brush Stroke",
            [],
            None,
            r##"{…paint.stroke params, with "points" and "size" in view pixels…,"projection":{"yaw":deg,"pitch":deg,"fov":deg (vertical),"width":px,"height":px}} — View › 360° View: a stroke drawn in the perspective view of a 2:1 equirectangular document, reprojected onto its pixels"##,
            has_paintable,
            crate::brush_cmds::paint_projected_stroke
        ),
    ]
}
