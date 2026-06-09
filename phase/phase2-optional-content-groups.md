# Phase 2 — Optional Content Groups (PDF Layers)

**Status:** Not started
**Effort:** ~4 weeks
**Tier gate:** Pro

## Context

Optional Content Groups (OCGs) are the PDF mechanism for layers — content that can be shown/hidden independently. Technical PDFs (CAD, maps, engineering drawings) use them heavily. Without OCG support, all layers render simultaneously and cannot be toggled. Specified in ISO 32000-1 §8.11.

## Step 1 — New Module `src/document/ocg.rs`

```rust
use crate::parser::{PdfDocument, PdfObject, PdfDict};
use crate::editor::PdfEditor;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct OcgLayer {
    pub id: u32,           // object ID of the OCG dict
    pub name: String,      // /Name
    pub visible: bool,     // true if ON in /OCProperties /D /ON list
}

/// List all Optional Content Groups defined in the document.
pub fn list_layers(doc: &PdfDocument) -> Result<Vec<OcgLayer>> {
    let trailer = &doc.trailer;
    let root = doc.resolve(trailer.get("Root").ok_or_else(|| PdfError::invalid_structure("no root"))?)?;
    let root_dict = root.as_dict().ok_or_else(|| PdfError::invalid_structure("root not dict"))?;

    let ocprops = match root_dict.get("OCProperties") {
        Some(o) => doc.resolve(o)?,
        None => return Ok(vec![]), // no layers
    };
    let ocprops_dict = ocprops.as_dict().ok_or_else(|| PdfError::invalid_structure("OCProperties not dict"))?;

    // /OCGs: array of all OCG references
    let ocgs = match ocprops_dict.get("OCGs") {
        Some(PdfObject::Array(a)) => a.clone(),
        _ => return Ok(vec![]),
    };

    // /D (default view config): /ON array lists which OCGs are visible by default
    let on_set: std::collections::HashSet<u32> = if let Some(d_obj) = ocprops_dict.get("D") {
        let d = doc.resolve(d_obj)?.into_dict().unwrap_or_default();
        if let Some(PdfObject::Array(on_arr)) = d.get("ON") {
            on_arr.iter().filter_map(|r| if let PdfObject::Reference(n,_) = r { Some(*n) } else { None }).collect()
        } else { std::collections::HashSet::new() }
    } else { std::collections::HashSet::new() };

    let mut layers = Vec::new();
    for ocg_ref in ocgs {
        let id = match &ocg_ref { PdfObject::Reference(n,_) => *n, _ => continue };
        let ocg_obj = doc.resolve(&ocg_ref)?;
        let ocg_dict = ocg_obj.as_dict().ok_or_else(|| PdfError::invalid_structure("OCG not dict"))?;
        let name = match ocg_dict.get("Name") {
            Some(PdfObject::String(b)) => String::from_utf8_lossy(b).to_string(),
            Some(PdfObject::Name(n)) => n.clone(),
            _ => format!("Layer {}", id),
        };
        // If /D has no /ON list, all OCGs are ON by default
        let visible = on_set.is_empty() || on_set.contains(&id);
        layers.push(OcgLayer { id, name, visible });
    }
    Ok(layers)
}

/// Set a layer's visibility in the document's default config (/OCProperties /D /ON).
pub fn set_layer_visible(editor: &mut PdfEditor, ocg_id: u32, visible: bool) -> Result<()> {
    crate::license::require(crate::license::Tier::Pro, "ocg_layers")?;
    let trailer = editor.doc.trailer.clone();
    let root = editor.get_object(match trailer.get("Root") { Some(PdfObject::Reference(n,_)) => *n, _ => return Err(PdfError::invalid_structure("no root")) })?;
    let mut root_dict = match root { PdfObject::Dictionary(d) => d, _ => return Err(PdfError::invalid_structure("root not dict")) };

    let ocprops_ref = root_dict.get("OCProperties").cloned().ok_or_else(|| PdfError::invalid_structure("no OCProperties"))?;
    let ocprops_id = match &ocprops_ref { PdfObject::Reference(n,_) => *n, _ => return Err(PdfError::invalid_structure("OCProperties not ref")) };
    let ocprops = editor.get_object(ocprops_id)?;
    let mut ocprops_dict = match ocprops { PdfObject::Dictionary(d) => d, _ => return Err(PdfError::invalid_structure("OCProperties not dict")) };

    // Update /D /ON list
    let d_ref = ocprops_dict.get("D").cloned().ok_or_else(|| PdfError::invalid_structure("no /D"))?;
    let d_id = match &d_ref { PdfObject::Reference(n,_) => *n, _ => return Err(PdfError::invalid_structure("/D not ref")) };
    let d_obj = editor.get_object(d_id)?;
    let mut d_dict = match d_obj { PdfObject::Dictionary(d) => d, _ => return Err(PdfError::invalid_structure("/D not dict")) };

    let mut on_list: Vec<PdfObject> = match d_dict.get("ON") {
        Some(PdfObject::Array(a)) => a.clone(),
        _ => {
            // Initialize /ON with all OCGs (all visible by default)
            let all_ocgs = match ocprops_dict.get("OCGs") {
                Some(PdfObject::Array(a)) => a.clone(),
                _ => vec![],
            };
            all_ocgs
        }
    };

    let ocg_ref = PdfObject::Reference(ocg_id, 0);
    let already_in_on = on_list.iter().any(|r| matches!(r, PdfObject::Reference(n,_) if *n == ocg_id));

    if visible && !already_in_on {
        on_list.push(ocg_ref);
    } else if !visible && already_in_on {
        on_list.retain(|r| !matches!(r, PdfObject::Reference(n,_) if *n == ocg_id));
    }

    d_dict.insert("ON".to_owned(), PdfObject::Array(on_list));
    editor.replace_object(d_id, PdfObject::Dictionary(d_dict));
    Ok(())
}

/// Create a new Optional Content Group (layer) and return its object ID.
pub fn create_layer(editor: &mut PdfEditor, name: &str) -> Result<u32> {
    crate::license::require(crate::license::Tier::Pro, "ocg_layers")?;
    let mut ocg_dict = PdfDict::new();
    ocg_dict.insert("Type".to_owned(), PdfObject::Name("OCG".to_owned()));
    ocg_dict.insert("Name".to_owned(), PdfObject::String(name.as_bytes().to_vec()));
    let ocg_id = editor.add_object(PdfObject::Dictionary(ocg_dict));
    // Add to /OCProperties /OCGs array
    // ... (get root → OCProperties → OCGs, append ocg_id, replace)
    // Add to /D /ON (visible by default)
    // ... (get /D, append to /ON)
    Ok(ocg_id)
}
```

## Step 2 — Layer Drawing in `src/writer/content_builder.rs`

Add to `ContentBuilder`:
```rust
/// Begin a marked-content sequence for an Optional Content Group.
/// `ocg_id` is the object ID of the OCG dict.
/// Emits: `/OC /MC0 BDC` (with properties dict referencing the OCG).
pub fn begin_layer(&mut self, ocg_id: u32) -> &mut Self {
    // In a full implementation this requires the OCG to be in page /Resources /Properties
    // For now emit a tagged BDC with the OCG reference
    self.ops.extend_from_slice(format!("/OC /OCG{} BDC\n", ocg_id).as_bytes());
    self
}

/// End a marked-content sequence.
pub fn end_layer(&mut self) -> &mut Self {
    self.ops.extend_from_slice(b"EMC\n");
    self
}
```

When `begin_layer()` is used, the page's `/Resources /Properties` dict must include the OCG reference:
```rust
// In page resource dict:
resources_properties.insert(format!("OCG{}", ocg_id), PdfObject::Reference(ocg_id, 0));
```

This is handled by the page editor when adding content to a layer.

## Step 3 — Renderer Support in `src/render/page_renderer.rs`

In the content interpreter dispatch for `BDC` (Begin Marked Content with Dict):
```rust
"BDC" => {
    // Check if this marks an OCG
    if let Some(props_dict) = get_properties_from_bdc_args(&args, resources) {
        if let Some(PdfObject::Name(oc_type)) = props_dict.get("Type") {
            if oc_type == "OCG" {
                let ocg_id = get_ocg_id(props_dict);
                let visible = ocg_visibility.get(&ocg_id).copied().unwrap_or(true);
                if !visible {
                    // Skip all content until matching EMC
                    skip_depth += 1;
                }
            }
        }
    }
}
"EMC" => {
    if skip_depth > 0 { skip_depth -= 1; }
}
```

The `ocg_visibility: HashMap<u32, bool>` is passed to the renderer from the document's `/OCProperties /D /ON` list.

## Step 4 — Update `src/document/mod.rs`

```rust
pub mod ocg;
pub use ocg::{OcgLayer, list_layers, set_layer_visible, create_layer};
```

## WASM in `src/wasm/editor.rs` and `src/wasm/document.rs`

```rust
// In WasmDocument:
#[wasm_bindgen]
pub fn list_layers(&self) -> Result<String, JsError> {
    let layers = crate::document::list_layers(&self.doc)
        .map_err(|e| JsError::new(&e.to_string()))?;
    // Return JSON: [{id, name, visible}]
    // ...
    Ok(json)
}

// In WasmEditor:
#[wasm_bindgen]
pub fn set_layer_visible(&mut self, ocg_id: u32, visible: bool) -> Result<(), JsError> {
    crate::document::set_layer_visible(&mut self.editor, ocg_id, visible)
        .map_err(|e| JsError::new(&e.to_string()))
}

#[wasm_bindgen]
pub fn create_layer(&mut self, name: &str) -> Result<u32, JsError> {
    crate::document::create_layer(&mut self.editor, name)
        .map_err(|e| JsError::new(&e.to_string()))
}
```

## Tests

```rust
#[test]
fn list_layers_returns_empty_for_plain_pdf() {
    let doc = PdfDocument::parse(include_bytes!("fixtures/minimal.pdf").to_vec()).unwrap();
    let layers = list_layers(&doc).unwrap();
    assert!(layers.is_empty());
}

#[test]
fn create_layer_adds_to_ocgs() {
    let data = include_bytes!("fixtures/minimal.pdf").to_vec();
    let mut editor = PdfEditor::open(data).unwrap();
    let layer_id = create_layer(&mut editor, "Background").unwrap();
    assert!(layer_id > 0);
    let saved = editor.save_append().unwrap();
    let doc2 = PdfDocument::parse(saved).unwrap();
    let layers = list_layers(&doc2).unwrap();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].name, "Background");
}
```

## Verification

```bash
cargo test -- ocg layer
cargo build --target wasm32-unknown-unknown --features wasm
```
