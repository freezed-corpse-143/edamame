//! Markdown list detection and structure editing: byte-oriented like `table_edit.rs`,
//! producing `EditDelta`s that `edit_ops` converts to char offsets.
//!
//! A "list" is a contiguous run of item lines at one indent and marker family.  Deeper
//! non-blank lines and interior blank runs followed by one belong to the item above; a
//! blank run before a same-indent marker, or any non-marker line at or below the list's
//! indent, ends the run.  So the cursor's list is always the innermost list at its own
//! indent.  [`parse`] holds types and parsers, [`edit`] the delta builders.

pub mod edit;
pub mod parse;

pub use edit::*;
pub use parse::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn info_at(source: &str, cursor_byte: usize) -> ListInfo {
        find_list_at(source, cursor_byte).expect("expected a list at cursor")
    }

    #[test]
    fn finds_simple_bullet_list() {
        let src = "- a\n- b\n- c\n";
        let info = info_at(src, 2);
        assert_eq!(info.items.len(), 3);
        assert_eq!(info.kind, MarkerKind::Bullet('-'));
        assert_eq!(info.indent, "");
    }

    #[test]
    fn finds_ordered_list_with_numbers() {
        let src = "1. one\n2. two\n3. three\n";
        let info = info_at(src, 5);
        assert_eq!(info.items.len(), 3);
        assert_eq!(info.kind, MarkerKind::Ordered('.'));
        assert_eq!(info.items[0].number, Some(1));
        assert_eq!(info.items[2].number, Some(3));
    }

    #[test]
    fn detects_task_items() {
        let src = "- [ ] todo\n- [x] done\n";
        let info = info_at(src, 3);
        assert_eq!(info.items[0].task, Some(false));
        assert_eq!(info.items[1].task, Some(true));
    }

    #[test]
    fn none_outside_list() {
        let src = "just text\n";
        assert!(find_list_at(src, 5).is_none());
    }

    #[test]
    fn nested_list_scoped_to_indent() {
        let src = "- outer\n  - inner1\n  - inner2\n- outer2\n";
        let info = info_at(src, 12);
        assert_eq!(info.items.len(), 2);
        assert_eq!(info.indent, "  ");
    }

    #[test]
    fn continue_item_at_end_of_line() {
        let src = "- foo\n";
        let info = info_at(src, 5);
        let res = continue_item(&info, src, 5).expect("continue");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n- \n");
        assert_eq!(res.cursor_byte, 8);
    }

    #[test]
    fn continue_renumbers_subsequent_ordered_items() {
        let src = "1. a\n2. b\n3. c\n";
        let info = info_at(src, 4);
        let res = continue_item(&info, src, 4).expect("continue");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "1. a\n2. \n3. b\n4. c\n");
    }

    #[test]
    fn exit_list_removes_empty_marker() {
        let src = "- foo\n- \n";
        let info = info_at(src, 8);
        let res = exit_list(&info, src, 8).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n\n");
    }

    #[test]
    fn exit_list_with_ordered_trailing_renumbers_from_one() {
        let src = "1. a\n2. \n3. b\n4. c\n";
        let info = info_at(src, 8);
        let res = exit_list(&info, src, 8).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "1. a\n\n1. b\n2. c\n");
        assert_eq!(res.cursor_byte, 5);
    }

    #[test]
    fn exit_list_with_bullet_trailing_keeps_items_unchanged() {
        let src = "- a\n- \n- b\n";
        let info = info_at(src, 5);
        let res = exit_list(&info, src, 5).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- a\n\n- b\n");
        assert_eq!(res.cursor_byte, 4);
    }

    #[test]
    fn exit_list_no_trailing_with_blank_above_strips_only_the_marker() {
        // Triple-`Enter` end state: `space_out_empty_item` already put the blank above.
        let src = "- foo\n\n- ";
        let info = info_at(src, 9);
        let res = exit_list(&info, src, 9).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n\n");
        assert_eq!(res.cursor_byte, 7);
    }

    #[test]
    fn exit_list_with_blank_above_and_ordered_trailing_renumbers() {
        let src = "1. a\n2. b\n\n3. \n4. c\n";
        let info = info_at(src, 12);
        let res = exit_list(&info, src, 12).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "1. a\n2. b\n\n1. c\n");
        assert_eq!(res.cursor_byte, 10);
    }

    #[test]
    fn space_out_empty_item_inserts_blank_line_above() {
        let src = "- foo\n- ";
        let info = info_at(src, 8);
        let res = space_out_empty_item(&info, src, 8).expect("space");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n\n- ");
        assert_eq!(res.cursor_byte, 9);
    }

    #[test]
    fn space_out_empty_item_rejects_non_empty_item() {
        let src = "- foo\n";
        let info = info_at(src, 5);
        assert!(space_out_empty_item(&info, src, 5).is_none());
    }

    #[test]
    fn is_blank_line_above_recognises_blank_predecessor() {
        assert!(is_blank_line_above("- foo", 0));
        assert!(is_blank_line_above("- foo\n\n- bar", 7));
        assert!(!is_blank_line_above("- foo\n- bar", 6));
        assert!(!is_blank_line_above("text\n- foo", 5));
    }

    #[test]
    fn toggle_checkbox_flips_state() {
        let src = "- [x] done\n";
        let info = info_at(src, 6);
        let res = toggle_checkbox(&info, src, 6).expect("toggle");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- [ ] done\n");
    }

    #[test]
    fn renumber_list_fixes_disordered_numbers() {
        let src = "1. a\n1. b\n1. c\n";
        let delta = renumber_ordered_runs_in_range(src, 0, src.len()).expect("renumber");
        let mut out = src.to_owned();
        out.replace_range(
            delta.offset..delta.offset + delta.removed.len(),
            &delta.inserted,
        );
        assert_eq!(out, "1. a\n2. b\n3. c\n");
    }

    #[test]
    fn renumber_list_noop_when_already_sequential() {
        let src = "1. a\n2. b\n3. c\n";
        assert!(renumber_ordered_runs_in_range(src, 0, src.len()).is_none());
    }

    #[test]
    fn renumber_range_spans_loose_list_blank_gaps() {
        let src = "1. a\n\n5. b\n\n2. c\n";
        let delta = renumber_ordered_runs_in_range(src, 0, src.len()).expect("renumber");
        let mut out = src.to_owned();
        out.replace_range(
            delta.offset..delta.offset + delta.removed.len(),
            &delta.inserted,
        );
        assert_eq!(out, "1. a\n\n2. b\n\n3. c\n");
    }

    #[test]
    fn continue_item_rejects_cursor_in_marker() {
        let src = "- foo\n";
        let info = info_at(src, 1);
        assert!(continue_item(&info, src, 1).is_none());
    }

    // ── Multi-line items ──────────────────────────────────────────────────

    #[test]
    fn find_list_includes_continuation_lines() {
        let src = "- a\n  cont\n- b\n";
        let info = info_at(src, 2);
        assert_eq!(info.items.len(), 2);
        assert_eq!(
            &src[info.items[0].start..info.items[0].end],
            "- a\n  cont\n"
        );
        assert_eq!(
            info.items[0].line_end, 3,
            "line_end stays a first-line fact"
        );
        assert_eq!(&src[info.items[1].start..info.items[1].end], "- b\n");
    }

    #[test]
    fn find_list_from_cursor_on_continuation_line() {
        let src = "- a\n  cont\n- b\n";
        let info = info_at(src, 7);
        assert_eq!(info.items.len(), 2);
        assert_eq!(cursor_item_idx(&info, 7), Some(0));
    }

    #[test]
    fn interior_blank_then_continuation_stays_one_item() {
        let src = "- a\n\n  cont\n- b\n";
        let info = info_at(src, 2);
        assert_eq!(info.items.len(), 2);
        assert_eq!(
            &src[info.items[0].start..info.items[0].end],
            "- a\n\n  cont\n",
            "the attached blank run and continuation belong to item 0"
        );
    }

    #[test]
    fn blank_before_same_level_marker_still_ends_scan() {
        let src = "- a\n\n- b\n";
        let info = info_at(src, 2);
        assert_eq!(info.items.len(), 1, "separator blank splits the lists");
        let info_b = info_at(src, 6);
        assert_eq!(info_b.items.len(), 1);
        assert_eq!(info_b.start, 5);
    }

    #[test]
    fn cursor_on_blank_separator_below_list_finds_nothing() {
        assert!(find_list_at("- a\n\n- b\n", 4).is_none());
        assert!(find_list_at("- a\n", 4).is_none());
        assert!(find_list_at("- a\n\n  cont\n", 4).is_some());
    }

    #[test]
    fn content_is_empty_false_with_continuation() {
        let src = "- \n  cont\n";
        let info = info_at(src, 2);
        assert!(!info.items[0].content_is_empty(src));
    }

    #[test]
    fn deeper_nested_marker_extends_outer_item() {
        let src = "- a\n  - child\n- b\n";
        let info = info_at(src, 2);
        assert_eq!(info.items.len(), 2);
        assert_eq!(
            &src[info.items[0].start..info.items[0].end],
            "- a\n  - child\n"
        );
        let nested = info_at(src, 6);
        assert_eq!(nested.indent, "  ");
        assert_eq!(nested.items.len(), 1);
    }

    #[test]
    fn cursor_on_flush_left_non_list_line_finds_nothing() {
        let src = "- a\npara\n";
        assert!(find_list_at(src, 5).is_none());
    }

    #[test]
    fn continuation_shaped_lines_without_marker_above_find_nothing() {
        let src = "para\n  indented\n";
        assert!(find_list_at(src, 7).is_none());
    }

    #[test]
    fn continue_item_mid_first_line_carries_continuations() {
        let src = "- ab\n  cont\n- c\n";
        let info = info_at(src, 3);
        let res = continue_item(&info, src, 3).expect("continues");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "- a\n- b\n  cont\n- c\n");
        assert_eq!(res.cursor_byte, 6);
    }

    #[test]
    fn continue_item_at_item_end_appends_sibling() {
        let src = "- a\n  cont\n";
        let info = info_at(src, 2);
        let res = continue_item(&info, src, 10).expect("continues");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "- a\n  cont\n- \n");
        assert_eq!(res.cursor_byte, 13);
    }

    #[test]
    fn continue_item_mid_continuation_returns_none() {
        let src = "- a\n  cont\n- b\n";
        let info = info_at(src, 7);
        assert!(continue_item(&info, src, 7).is_none());
    }

    #[test]
    fn indent_item_rejects_first_item() {
        let src = "- a\n- b\n";
        let info = info_at(src, 2);
        assert!(indent_item(&info, src, 2, 4).is_none());
        let nested = "- top\n  - x\n  - y\n";
        let info = info_at(nested, 10);
        assert!(indent_item(&info, nested, 10, 4).is_none());
    }

    #[test]
    fn indent_item_shifts_all_item_lines_bullet() {
        let src = "- a\n- b\n  cont\n- c\n";
        let info = info_at(src, 5);
        let res = indent_item(&info, src, 5, 4).expect("indents");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "- a\n    - b\n      cont\n- c\n");
        assert_eq!(
            res.cursor_byte, 9,
            "cursor tracks its char on the marker line"
        );
    }

    #[test]
    fn indent_item_shifts_all_item_lines_ordered() {
        let src = "1. a\n2. b\n   cont\n3. c\n";
        let info = info_at(src, 6);
        let res = indent_item(&info, src, 6, 4).expect("indents");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "1. a\n    1. b\n       cont\n2. c\n");
    }

    #[test]
    fn outdent_item_shifts_all_item_lines() {
        let src = "- top\n    - b\n      cont\n";
        let info = info_at(src, 11);
        let res = outdent_item(&info, src, 11, 4).expect("outdents");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "- top\n- b\n  cont\n");
    }

    #[test]
    fn exit_list_preserves_trailing_multiline_items() {
        let src = "1. a\n\n2. \n3. b\n   cont\n";
        let info = info_at(src, 8);
        let res = exit_list(&info, src, 8).expect("exits");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "1. a\n\n1. b\n   cont\n");
    }

    #[test]
    fn renumber_range_crosses_continuation_lines() {
        let src = "1. a\n   cont\n1. b\n";
        let delta = renumber_ordered_runs_in_range(src, 0, src.len()).expect("renumbers");
        let mut out = src.to_owned();
        out.replace_range(delta.offset..delta.offset + delta.removed.len(), "");
        out.insert_str(delta.offset, &delta.inserted);
        assert_eq!(out, "1. a\n   cont\n2. b\n");
    }
}
