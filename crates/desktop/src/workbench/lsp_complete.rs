//! Completion and signature help of a file tab (slice E-12b): the code view asks
//! (`hutan-code-view`'s `completion.rs`), the workbench sends `lsp.completion`,
//! `lsp.completion_resolve` and `lsp.signature_help` through the window's [`LspStore`] (after
//! the edits on their way) and hands the answers back. A newer request supersedes an older one
//! in the daemon; the superseded answer (`conflict`) is dropped like any stale one.

use gpui::{Context, Entity};
use hutan_client::ClientError;
use hutan_code_view::{
    CodeView, CompletionEdit, CompletionItem, CompletionKind, CompletionList, Signature,
    SignatureHelp, Utf16Point,
};
use hutan_protocol::ErrorCode;
use hutan_protocol::lsp::{
    LspCompletionItem, LspCompletionKind, LspCompletionList, LspSignatureHelp, LspTextEdit,
};

use super::file_view::FileView;
use super::lsp::position_of;
use super::view::Workbench;

fn kind_of(kind: LspCompletionKind) -> CompletionKind {
    use CompletionKind as K;
    use LspCompletionKind as L;
    match kind {
        L::Text => K::Text,
        L::Method => K::Method,
        L::Function => K::Function,
        L::Constructor => K::Constructor,
        L::Field => K::Field,
        L::Variable => K::Variable,
        L::Class => K::Class,
        L::Interface => K::Interface,
        L::Module => K::Module,
        L::Property => K::Property,
        L::Unit => K::Unit,
        L::Value => K::Value,
        L::Enum => K::Enum,
        L::Keyword => K::Keyword,
        L::Snippet => K::Snippet,
        L::Color => K::Color,
        L::File => K::File,
        L::Reference => K::Reference,
        L::Folder => K::Folder,
        L::EnumMember => K::EnumMember,
        L::Constant => K::Constant,
        L::Struct => K::Struct,
        L::Event => K::Event,
        L::Operator => K::Operator,
        L::TypeParameter => K::TypeParameter,
        L::Other => K::Other,
    }
}

fn edit_of(edit: LspTextEdit) -> CompletionEdit {
    let point = |p: hutan_protocol::lsp::LspPosition| Utf16Point {
        line: p.line as usize,
        character: p.character as usize,
    };
    CompletionEdit {
        start: point(edit.range.start),
        end: point(edit.range.end),
        text: edit.new_text,
    }
}

/// A completion item of the wire as the code view takes it.
pub fn item_of(item: LspCompletionItem) -> CompletionItem {
    CompletionItem {
        label: item.label,
        kind: item.kind.map(kind_of),
        detail: item.detail,
        documentation: item.documentation,
        sort_text: item.sort_text,
        filter_text: item.filter_text,
        insert_text: item.insert_text,
        snippet: item.snippet,
        edit: item.edit.map(edit_of),
        additional_edits: item.additional_edits.into_iter().map(edit_of).collect(),
        preselect: item.preselect,
        deprecated: item.deprecated,
        resolvable: item.resolvable,
    }
}

fn list_of(list: LspCompletionList) -> CompletionList {
    CompletionList {
        id: list.completion_id,
        items: list.items.into_iter().map(item_of).collect(),
        incomplete: list.incomplete,
    }
}

fn help_of(help: LspSignatureHelp) -> SignatureHelp {
    SignatureHelp {
        active: help.active_signature as usize,
        signatures: help
            .signatures
            .into_iter()
            .map(|s| Signature {
                parameters: s
                    .parameters
                    .iter()
                    .map(|p| p.start as usize..p.end as usize)
                    .collect(),
                parameter_docs: s.parameters.into_iter().map(|p| p.documentation).collect(),
                active_parameter: s.active_parameter.map(|i| i as usize),
                label: s.label,
                documentation: s.documentation,
            })
            .collect(),
    }
}

/// The daemon cancelled the request for a newer one of the same kind.
fn superseded(error: &ClientError) -> bool {
    matches!(error, ClientError::Remote(body)
        if body.code == ErrorCode::Conflict
            && body.data.as_ref().is_some_and(|d| d["reason"] == "superseded"))
}

impl Workbench {
    /// Tells every file tab's code view whether its file has completion, and the triggers.
    pub(crate) fn refresh_completion_triggers(&mut self, cx: &mut Context<Self>) {
        let views: Vec<Entity<FileView>> = self.file_views.values().cloned().collect();
        for view in views {
            let (code, path) = {
                let view = view.read(cx);
                (
                    view.code().clone(),
                    view.buffer().read(cx).path().display().to_string(),
                )
            };
            let triggers = self.lsp.read(cx).completion_triggers(&path);
            code.update(cx, |code, _| code.set_completion_triggers(triggers));
        }
    }

    fn code_and_place(
        view: &Entity<FileView>,
        offset: usize,
        cx: &Context<Self>,
    ) -> (Entity<CodeView>, String, hutan_protocol::lsp::LspPosition) {
        let view = view.read(cx);
        let buffer = view.buffer().read(cx);
        (
            view.code().clone(),
            buffer.path().display().to_string(),
            position_of(buffer.text(), offset),
        )
    }

    /// The code view of `view` asks for completions at byte `offset`.
    pub(crate) fn lsp_completion(
        &mut self,
        view: &Entity<FileView>,
        request: u64,
        offset: usize,
        trigger: Option<char>,
        incomplete: bool,
        cx: &mut Context<Self>,
    ) {
        let (code, path, position) = Self::code_and_place(view, offset, cx);
        let Some(call) = self.lsp.update(cx, |lsp, cx| {
            lsp.completion(&path, position, trigger, incomplete, cx)
        }) else {
            code.update(cx, |code, cx| code.set_completions(request, None, cx));
            return;
        };
        cx.spawn(async move |_, cx| {
            let list = match call.await {
                Ok(Some(list)) => Some(list_of(list)),
                // A newer request replaced it; its answer is the one that counts.
                Ok(None) => return,
                Err(error) if superseded(&error) => return,
                Err(error) => {
                    tracing::debug!(%error, "lsp.completion failed");
                    None
                }
            };
            code.update(cx, |code, cx| code.set_completions(request, list, cx));
        })
        .detach();
    }

    /// The code view of `view` asks for more about item `index` of list `list`.
    pub(crate) fn lsp_completion_resolve(
        &mut self,
        view: &Entity<FileView>,
        list: u64,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let code = view.read(cx).code().clone();
        let Ok(wire_index) = u32::try_from(index) else {
            return;
        };
        let call = self
            .lsp
            .update(cx, |lsp, cx| lsp.completion_resolve(list, wire_index, cx));
        cx.spawn(async move |_, cx| {
            let item = match call.await {
                Ok(item) => Some(item_of(item)),
                Err(error) if superseded(&error) => return,
                Err(error) => {
                    tracing::debug!(%error, "lsp.completion_resolve failed");
                    None
                }
            };
            code.update(cx, |code, cx| {
                code.set_resolved_completion(list, index, item, cx)
            });
        })
        .detach();
    }

    /// The code view of `view` asks for signature help at byte `offset`.
    pub(crate) fn lsp_signature_help(
        &mut self,
        view: &Entity<FileView>,
        request: u64,
        offset: usize,
        trigger: Option<char>,
        retrigger: bool,
        cx: &mut Context<Self>,
    ) {
        let (code, path, position) = Self::code_and_place(view, offset, cx);
        let Some(call) = self.lsp.update(cx, |lsp, cx| {
            lsp.signature_help(&path, position, trigger, retrigger, cx)
        }) else {
            code.update(cx, |code, cx| code.set_signature_help(request, None, cx));
            return;
        };
        cx.spawn(async move |_, cx| {
            let help = match call.await {
                Ok(Some(help)) => Some(help_of(help)),
                Ok(None) => return,
                Err(error) if superseded(&error) => return,
                Err(error) => {
                    tracing::debug!(%error, "lsp.signature_help failed");
                    None
                }
            };
            code.update(cx, |code, cx| code.set_signature_help(request, help, cx));
        })
        .detach();
    }
}
