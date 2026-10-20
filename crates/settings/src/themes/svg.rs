//! Strict, bounded monochrome SVG subset. Only reconstructed, validated XML reaches GPUI.

use std::fmt::Write as _;

pub const MAX_SVG_BYTES: usize = 64 * 1024;
const MAX_ATTRIBUTE: usize = 32 * 1024;
const MAX_SEGMENTS: usize = 16_384;

fn number(n: f64) -> Result<(), String> {
    if n.is_finite() && n.abs() <= 1e6 {
        Ok(())
    } else {
        Err("coordinates must be finite and at most 1e6".into())
    }
}

fn numbers(value: &str) -> Result<Vec<f64>, String> {
    let values: Vec<_> = svgtypes::NumberListParser::from(value)
        .collect::<Result<_, _>>()
        .map_err(|_| "invalid numeric geometry")?;
    if values.is_empty() {
        return Err("empty numeric geometry".into());
    }
    for &n in &values {
        number(n)?;
    }
    Ok(values)
}

fn path(value: &str, segments: &mut usize) -> Result<(), String> {
    use svgtypes::PathSegment::*;
    let mut count = 0;
    for segment in svgtypes::PathParser::from(value) {
        let segment = segment.map_err(|_| "invalid path geometry")?;
        count += 1;
        *segments += 1;
        if *segments > MAX_SEGMENTS {
            return Err("more than 16384 path segments".into());
        }
        let values: &[f64] = match &segment {
            MoveTo { x, y, .. } | LineTo { x, y, .. } | SmoothQuadratic { x, y, .. } => &[*x, *y],
            HorizontalLineTo { x, .. } => &[*x],
            VerticalLineTo { y, .. } => &[*y],
            CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
                ..
            } => &[*x1, *y1, *x2, *y2, *x, *y],
            SmoothCurveTo { x2, y2, x, y, .. } => &[*x2, *y2, *x, *y],
            Quadratic { x1, y1, x, y, .. } => &[*x1, *y1, *x, *y],
            EllipticalArc {
                rx,
                ry,
                x_axis_rotation,
                x,
                y,
                ..
            } => &[*rx, *ry, *x_axis_rotation, *x, *y],
            ClosePath { .. } => &[],
        };
        for &n in values {
            number(n)?;
        }
    }
    if count == 0 {
        return Err("empty path".into());
    }
    Ok(())
}

fn transform(value: &str) -> Result<(), String> {
    use svgtypes::TransformListToken::*;
    let mut count = 0;
    for token in svgtypes::TransformListParser::from(value) {
        count += 1;
        let token = token.map_err(|_| "invalid transform")?;
        let values: &[f64] = match &token {
            Matrix { a, b, c, d, e, f } => &[*a, *b, *c, *d, *e, *f],
            Translate { tx, ty } => &[*tx, *ty],
            Scale { sx, sy } => &[*sx, *sy],
            Rotate { angle } | SkewX { angle } | SkewY { angle } => &[*angle],
        };
        for &n in values {
            number(n)?;
        }
    }
    if count == 0 {
        return Err("empty transform".into());
    }
    Ok(())
}

/// Reject unsupported markup rather than trying to remove dangerous fragments. No DTD/entity
/// resolution, URLs, CSS, text, image, filters, use, animation, or namespace tricks.
pub fn sanitize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_SVG_BYTES {
        return Err("SVG exceeds 64 KiB".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "SVG is not UTF-8")?;
    if text.contains("<!") || text.contains("<?") || text.contains('&') {
        return Err(
            "declarations, entities, processing instructions and comments are not supported".into(),
        );
    }
    let doc = roxmltree::Document::parse(text).map_err(|_| "invalid SVG XML")?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" {
        return Err("root must be svg".into());
    }
    let viewbox = numbers(root.attribute("viewBox").ok_or("square viewBox required")?)?;
    if viewbox.len() != 4 || viewbox[2] <= 0.0 || viewbox[2] > 4096.0 || viewbox[2] != viewbox[3] {
        return Err("viewBox must be positive, square and at most 4096".into());
    }
    let mut state = Emit {
        out: String::with_capacity(text.len()),
        segments: 0,
        elements: 0,
        painted: false,
    };
    emit(root, root, 1, &mut state)?;
    if state.elements < 2 || !state.painted {
        return Err("currentColor artwork required".into());
    }
    Ok(state.out.into_bytes())
}

struct Emit {
    out: String,
    segments: usize,
    elements: usize,
    painted: bool,
}

fn emit(
    node: roxmltree::Node<'_, '_>,
    root: roxmltree::Node<'_, '_>,
    depth: usize,
    state: &mut Emit,
) -> Result<(), String> {
    const SVG: &str = "http://www.w3.org/2000/svg";
    state.elements += 1;
    if state.elements > 2048 || depth > 16 {
        return Err("SVG element/depth limit exceeded".into());
    }
    let name = node.tag_name().name();
    if !matches!(
        name,
        "svg" | "g" | "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon"
    ) || (name == "svg" && node != root)
    {
        return Err(format!("unsupported SVG element `{name}`"));
    }
    if node.tag_name().namespace().is_some_and(|ns| ns != SVG)
        || node.namespaces().any(|ns| {
            ns.name().is_some_and(|prefix| prefix != "xml")
                || (ns.name().is_none() && ns.uri() != SVG)
        })
    {
        return Err("foreign SVG namespace refused".into());
    }
    let _ = write!(state.out, "<{name}");
    if node == root {
        state.out.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
    }
    for attr in node.attributes() {
        let key = attr.name();
        let value = attr.value();
        if attr.namespace().is_some()
            || value.len() > MAX_ATTRIBUTE
            || value.contains(['<', '>', '&', '"', '\''])
        {
            return Err("unsupported namespace, escaping or oversized SVG attribute".into());
        }
        match key {
            "fill" | "stroke" => {
                if !matches!(value, "currentColor" | "none") {
                    return Err("paint must be currentColor or none".into());
                }
                state.painted |= value == "currentColor";
            }
            "d" if name == "path" => path(value, &mut state.segments)?,
            "transform" => transform(value)?,
            "viewBox" if node == root => {}
            "width" | "height" if node == root => {
                numbers(value)?;
                continue;
            }
            "x" | "y" | "x1" | "y1" | "x2" | "y2" | "cx" | "cy" | "r" | "rx" | "ry" | "width"
            | "height" | "stroke-width" | "stroke-miterlimit" => {
                if numbers(value)?.len() != 1 {
                    return Err("expected one geometry number".into());
                }
            }
            "points" if matches!(name, "polyline" | "polygon") => {
                let values = numbers(value)?;
                state.segments += values.len() / 2;
                if !values.len().is_multiple_of(2) || state.segments > MAX_SEGMENTS {
                    return Err("invalid or oversized points".into());
                }
            }
            "opacity" | "fill-opacity" | "stroke-opacity" => {
                let values = numbers(value)?;
                if values.len() != 1 || !(0.0..=1.0).contains(&values[0]) {
                    return Err("opacity must be 0..1".into());
                }
            }
            "fill-rule" | "clip-rule" if matches!(value, "nonzero" | "evenodd") => {}
            "stroke-linecap" if matches!(value, "butt" | "round" | "square") => {}
            "stroke-linejoin" if matches!(value, "miter" | "round" | "bevel") => {}
            _ => return Err(format!("unsupported SVG attribute `{key}`")),
        }
        let _ = write!(state.out, " {key}=\"{value}\"");
    }
    state.out.push('>');
    for child in node.children() {
        if child.is_element() {
            emit(child, root, depth + 1, state)?;
        } else if child.is_text() && !child.text().unwrap_or_default().trim().is_empty() {
            return Err("SVG text refused".into());
        }
    }
    let _ = write!(state.out, "</{name}>");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn svg(body: &str) -> String {
        format!("<svg viewBox=\"0 0 24 24\" fill=\"currentColor\">{body}</svg>")
    }
    #[test]
    fn malicious_markup_is_rejected() {
        for body in [
            "<script/>",
            "<path onclick=\"alert(1)\"/>",
            "<image href=\"https://example.com/a\"/>",
            "<image href=\"data:image/png;base64,a\"/>",
            "<style/>",
            "<path style=\"fill:red\"/>",
            "<path fill=\"url(https://example.com)\"/>",
            "<text>Hi</text>",
            "<font/>",
            "<filter/>",
            "<use/>",
            "<foreignObject/>",
            "<animate/>",
            "<path d=\"M1e99 0\"/>",
            "<path d=\"MNaN 0\"/>",
            "<path href=\"#a\" d=\"M0 0L1 1\"/>",
            "<path xlink:href=\"https://example.com\" d=\"M0 0L1 1\"/>",
            "<mask/>",
            "<pattern/>",
            "<linearGradient/>",
            "<path fill=\"url(#a)\" d=\"M0 0L1 1\"/>",
            "<path fill=\"red\" d=\"M0 0L1 1\"/>",
            "<path d=\"&#77;0 0L1 1\" id=\"&unknown;\"/>",
            "<g xmlns=\"https://example.com/other\"/>",
        ] {
            assert!(sanitize(svg(body).as_bytes()).is_err(), "{body}");
        }
        for prefix in [
            "<!DOCTYPE svg>",
            "<!ENTITY a SYSTEM 'file:///etc/passwd'>",
            "<?xml-stylesheet href='https://example.com'?>",
            "<!DOCTYPE svg [<!ENTITY e 'M0 0L9 9'>]>",
        ] {
            assert!(
                sanitize(format!("{prefix}{}", svg("<path d=\"M0 0L1 1\"/>")).as_bytes()).is_err()
            );
        }
    }
    #[test]
    fn geometry_and_complexity_are_bounded() {
        for value in ["0 0 20 24", "0 0 -1 -1", "0 0 4097 4097", "0 0 inf inf"] {
            assert!(
                sanitize(
                    format!(
                        "<svg viewBox=\"{value}\" fill=\"currentColor\"><circle r=\"1\"/></svg>"
                    )
                    .as_bytes()
                )
                .is_err()
            );
        }
        assert!(
            sanitize(svg(&format!("<path d=\"M0 0{}\"/>", "h1".repeat(16385))).as_bytes()).is_err()
        );
        assert!(sanitize(svg(&"<g>".repeat(17)).as_bytes()).is_err());
        assert!(sanitize(svg(&"<circle r=\"1\"/>".repeat(2048)).as_bytes()).is_err());
        assert!(sanitize(&vec![b' '; MAX_SVG_BYTES + 1]).is_err());
    }
    #[test]
    fn safe_strokes_and_filled_paths_are_normalized() {
        for body in [
            "<path d=\"M0 0L1 1Z\"/>",
            "<circle cx=\"12\" cy=\"12\" r=\"5\" stroke=\"currentColor\" fill=\"none\"/>",
        ] {
            let bytes = sanitize(svg(body).as_bytes()).unwrap();
            assert!(
                std::str::from_utf8(&bytes)
                    .unwrap()
                    .contains("currentColor")
            );
            roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
        }
    }
}
