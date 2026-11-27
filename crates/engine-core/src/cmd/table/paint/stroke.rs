use crate::cmd::{CommandSpec, has_doc, has_paintable};

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
        cmd!(
            "paint.projectedTool",
            "Projected Tool Stroke",
            [],
            None,
            r##"{"command":"paint.healingBrush|paint.spotHealing|paint.patch|paint.pencil|paint.mixerBrush|paint.colorReplacement|paint.historyBrush|paint.patternStamp|paint.backgroundEraser|paint.remove|paint.blur|paint.sharpen|paint.smudge|paint.dodge|paint.burn|paint.sponge|paint.gradient|paint.bucket","params":{…that command's params, without points…},"points":[[x,y,pressure],…] (view px),"rotation":[9 numbers]? (Healing/Patch: destination→source on the sphere),"projection":{"yaw","pitch","fov","width","height"}} — View › 360° View: a painting or retouch tool stroke drawn in the perspective view, reprojected onto the equirect pixels; the Gradient (first point to last) and the Paint Bucket (first point) cover the whole latlong, the bucket flooding across the ±180° seam"##,
            has_paintable,
            crate::projected_tool::projected_tool
        ),
        cmd!(
            "select.projected",
            "Projected Selection",
            [],
            None,
            r##"{"kind":"rect|ellipse|lasso|wand|quick|object|row|column","rect":[x0,y0,x1,y1],"points":[[x,y],…],"mode":"replace|add|subtract|intersect","feather":px,"antiAlias":bool,"tolerance":n,"contiguous":bool,"size":px,"sampleAllLayers":bool,"projection":{"yaw":deg,"pitch":deg,"fov":deg,"width":px,"height":px}} — View › 360° View: a selection drawn in the perspective view, reprojected onto the equirect selection"##,
            has_doc,
            crate::projected_select::select_projected
        ),
        cmd!(
            "layer.projectedRotate",
            "Move",
            [],
            None,
            r##"{"rotation":[9 numbers] (row-major, from where the drag started to where it ended)} — View › 360° View: the Move tool turns the active layer (or the selected part, with the selection) across the sphere"##,
            has_paintable,
            crate::projected_transform::projected_rotate
        ),
        cmd!(
            "layer.projectedPlace",
            "Free Transform",
            [],
            None,
            r##"{"source":"layer|clipboard","rect":[x0,y0,x1,y1] (layer: the view-px source),"quad":[[x,y]×4] (tl,tr,br,bl view px),"projection":{"yaw","pitch","fov","width","height"}} — View › 360° View: Free Transform of the active layer's content, or Paste as a new layer, mapped through the view onto the equirect pixels"##,
            has_doc,
            crate::projected_transform::projected_place
        ),
        cmd!(
            "layer.projectedRender",
            "Type",
            [],
            None,
            r##"{"command":"type.create|shape.create","params":{…that command's params, in view px…},"projection":{"yaw","pitch","fov","width","height"}} — View › 360° View: the Type and shape tools; the text or shape is rendered upright where it was made in the view and placed on the latlong as a new pixel layer named after it"##,
            has_doc,
            crate::projected_transform::projected_render
        ),
        cmd!(
            "path.projectedSet",
            "Work Path",
            [],
            None,
            r##"{…path.set params, with "path" in view px…,"projection":{"yaw","pitch","fov","width","height"}} — View › 360° View: a path drawn with the Pen tools in the view, stored on the latlong where it was drawn (x may run past an edge across the ±180° seam; "wrap":true on path.toSelection and path.fill counts those parts on the other side)"##,
            has_doc,
            crate::projected_path::projected_set
        ),
        cmd!(
            "image.rotatePanorama",
            "Rotate Panorama",
            [],
            None,
            r##"{"yaw":deg (right)=0,"pitch":deg (up)=0,"roll":deg (clockwise)=0} — Image › Rotate Panorama: turns the whole latlong on its sphere, every pixel layer and mask; the selection is dropped"##,
            has_doc,
            crate::projected_transform::rotate_panorama
        ),
    ]
}
