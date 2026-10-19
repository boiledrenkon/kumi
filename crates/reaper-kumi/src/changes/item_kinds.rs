//! The kinds of change to items, takes, lanes, razor areas and MIDI events (M4), and the module each is made in.

use serde_json::Value;

use super::{items, Applied, Ctx, Done, Kind, Preview, Restore, Restored};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Which {
    ItemAdd,
    ItemImport,
    ItemMove,
    ItemSplit,
    ItemTrim,
    ItemFade,
    ItemProps,
    ItemGroup,
    ItemDelete,
    ItemCopy,
}

/// The kind a name is, if it is one of these.
pub(super) fn kind(name: &str) -> Option<Kind> {
    match name {
        "item-add" => Some(Kind::Items(Which::ItemAdd)),
        "item-import" => Some(Kind::Items(Which::ItemImport)),
        "item-move" => Some(Kind::Items(Which::ItemMove)),
        "item-split" => Some(Kind::Items(Which::ItemSplit)),
        "item-trim" => Some(Kind::Items(Which::ItemTrim)),
        "item-fade" => Some(Kind::Items(Which::ItemFade)),
        "item-props" => Some(Kind::Items(Which::ItemProps)),
        "item-group" => Some(Kind::Items(Which::ItemGroup)),
        "item-delete" => Some(Kind::Items(Which::ItemDelete)),
        "item-duplicate" => Some(Kind::Items(Which::ItemCopy)),
        _ => None,
    }
}

impl Which {
    pub(super) fn preview(self, ctx: &Ctx, input: &Value) -> Done<Preview> {
        match self {
            Which::ItemAdd => items::make_preview(ctx, input, false),
            Which::ItemImport => items::make_preview(ctx, input, true),
            Which::ItemMove => items::move_preview(ctx, input),
            Which::ItemSplit => items::split_preview(ctx, input),
            Which::ItemTrim => items::trim_preview(ctx, input),
            Which::ItemFade => items::fade_preview(ctx, input),
            Which::ItemProps => items::props_preview(ctx, input),
            Which::ItemGroup => items::group_preview(ctx, input),
            Which::ItemDelete => items::delete_preview(ctx, input),
            Which::ItemCopy => items::copy_preview(ctx, input),
        }
    }

    pub(super) fn apply(self, ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
        match self {
            Which::ItemAdd => items::make_apply(ctx, input, fence, false),
            Which::ItemImport => items::make_apply(ctx, input, fence, true),
            Which::ItemMove => items::move_apply(ctx, input, fence),
            Which::ItemSplit => items::split_apply(ctx, input, fence),
            Which::ItemTrim => items::trim_apply(ctx, input, fence),
            Which::ItemFade => items::fade_apply(ctx, input, fence),
            Which::ItemProps => items::props_apply(ctx, input, fence),
            Which::ItemGroup => items::group_apply(ctx, input, fence),
            Which::ItemDelete => items::delete_apply(ctx, input, fence),
            Which::ItemCopy => items::copy_apply(ctx, input, fence),
        }
    }

    pub(super) fn restore(self, ctx: &Ctx, restore: &Restore) -> Done<Restored> {
        match self {
            Which::ItemAdd | Which::ItemImport | Which::ItemCopy => items::created_restore(ctx, restore),
            Which::ItemMove => items::move_restore(ctx, restore),
            Which::ItemSplit => items::split_restore(ctx, restore),
            Which::ItemTrim => items::trim_restore(ctx, restore),
            Which::ItemFade => items::fade_restore(ctx, restore),
            Which::ItemProps => items::props_restore(ctx, restore),
            Which::ItemGroup => items::group_restore(ctx, restore),
            Which::ItemDelete => items::delete_restore(ctx, restore),
        }
    }
}
