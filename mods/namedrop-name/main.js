import { View, div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, Button } from "gpui-base";
import { focused, terminals, hc, exec, home, props, closeOverlay, theme } from "herdr";
import { chooseTarget, saveName } from "./naming.js";

export default class NameTerminal extends View {
  init() {
    this.address = chooseTarget(props(), focused());
    this.state = { kind: this.address ? "editing" : "unavailable", error: null };
    if (!this.address) return;
    const name = terminals().find((terminal) => terminal.address === this.address)?.labels?.["namedrop.name"] ?? "";
    this.input = InputState.new({ value: name, placeholder: "Code Reviewer" });
    if (name) this.input.set_selected_range({ start: 0, end: name.length });
    this.input.on("change", (_e, cx) => {
      if (this.state.kind !== "editing") return;
      this.state = { kind: "editing", error: null };
      cx.notify();
    });
    this.input.on("submit", (_e, cx) => this.submit(cx));
    this.input.focus();
  }

  dismiss() {
    const overlay = props()?.overlay;
    if (overlay) closeOverlay(overlay);
  }

  submit(cx) {
    if (this.state.kind !== "editing") return;
    this.state = { kind: "saving", alias: this.input.value() };
    cx.notify();
    cx.spawn(async (cx) => {
      const result = await saveName({ target: this.address, alias: this.input.value(), terminals, home, exec, hc });
      if (result.ok) {
        this.state = { kind: "closed" };
        this.dismiss();
        return;
      }
      this.state = { kind: "editing", error: result.error };
      cx.notify();
      this.input.focus();
    });
  }

  render() {
    const t = theme();
    const saving = this.state.kind === "saving";
    const root = v_flex().id("root").w_full().p(18).gap(12)
      .bg(t.popover).text_color(t.foreground)
      .child(div().text_size(15).font_semibold().child("Name this terminal"));
    if (this.state.kind === "unavailable") {
      return root.child(div().text_size(12).text_color(t.muted)
        .child("No terminal is focused. Close this dialog and focus a terminal before naming it."))
        .child(Button.new("close").h(32).px(14).rounded(8).bg(t.raised).text_size(12.5)
          .on_click(() => this.dismiss()).child("Close"));
    }
    return root
      .child(div().text_size(11.5).text_color(t.muted).child(`Alias for ${this.address}`))
      .child((saving ? div().flex().items_center().child(this.state.alias) : Input.new(this.input))
        .h(36).px(10).rounded(9).border_1()
        .border_color(this.state.error ? t.blocked : t.input).bg(t.background)
        .text_size(13).text_color(t.foreground))
      .child(div().text_size(11.5).text_color(t.muted).child("Code Reviewer becomes @code-reviewer."))
      .when(Boolean(this.state.error), (el) => el.child(div().text_size(12).text_color(t.blocked).child(this.state.error)))
      .child(h_flex().pt(8).gap(8).justify_end()
        .child(Button.new("cancel").h(32).px(14).rounded(8).bg(t.raised).text_size(12.5)
          .hover((s) => s.bg(t.hover)).focus((s) => s.bg(t.selected))
          .on_click(() => this.dismiss()).child("Cancel"))
        .child(Button.new("save").h(32).px(14).rounded(8).bg(t.solid).text_color(t.solid_foreground)
          .text_size(12.5).font_medium().cursor_pointer().disabled(saving)
          .when(saving, (el) => el.opacity(0.6)).hover((s) => s.opacity(0.88)).focus((s) => s.border_1().border_color(t.accent))
          .on_click((_e, cx) => this.submit(cx)).child(saving ? "Saving…" : "Save name")));
  }
}
