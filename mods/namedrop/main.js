import { View, div, image, svg } from "gpui-kit";
import { v_flex, h_flex, Button } from "gpui-base";
import { terminals, panes, spaces, placement, theme, run, exec, props, closeOverlay } from "herdr";
import { roster } from "./roster.js";

const releases = "https://github.com/ericbust/namedrop/releases";

export default class Namedrop extends View {
  init(_props, cx) {
    this.install = "checking";
    cx.spawn(async (cx) => {
      try {
        await exec("namedrop", ["--version"]);
        this.install = "available";
      } catch {
        this.install = "missing";
      }
      cx.notify();
    });
  }

  row(row, edge, t) {
    const body = h_flex().flex_1().min_w(0).items_center().gap(8);
    if (row.program?.icon) {
      body.child(row.program.tint
        ? svg(row.program.icon).size(18).flex_none().text_color(t.foreground)
        : image(row.program.icon).w(18).h(18).flex_none());
    }
    body.child(v_flex().flex_1().min_w(0).gap(2)
      .child(div().truncate().font_medium().child(`@${row.alias}`))
      .child(div().truncate().text_size(11).text_color(t.muted)
        .child(row.where ?? row.program?.name ?? row.address)));

    const button = Button.new(`terminal-${row.address}`).w_full().flex().items_center()
      .gap(8).px(10).py(8).rounded(8).text_size(13).text_color(t.foreground)
      .cursor_pointer().hover((s) => s.bg(t.hover)).focus((s) => s.bg(t.selected))
      .accessibility_label(`@${row.alias}, ${row.status ?? "no status reported"}, ${row.where ?? row.address}`)
      .tooltip(`${row.shown ? "Focus" : "Peek at"} @${row.alias}`)
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click(() => run(row.shown ? "pane.focus" : "terminal.peek", { terminal: row.address }));
    if (edge === "top" || edge === "bottom") button.flex_col();
    const dot = row.status ? div().size(7).flex_none().rounded(4).bg(t[row.status]) : null;
    if (dot && (edge === "left" || edge === "top")) button.child(dot);
    button.child(body);
    if (dot && (edge === "right" || edge === "bottom")) button.child(dot);
    return button;
  }

  render() {
    const t = theme();
    const rows = roster(terminals(), panes(), spaces());
    const edge = placement()?.edge ?? "right";
    return v_flex().id("root").w_full().min_w(280).p(8).gap(4).text_color(t.foreground)
      .on_size_change((size, cx) => { this.size = size; cx.notify(); })
      .child(h_flex().px(10).py(6).items_center().gap(8)
        .child(div().flex_1().text_size(14).font_semibold().child("Namedrop"))
        .when(Boolean(props()?.overlay), (el) => el.child(Button.new("close-roster").px(6).h(26)
          .rounded(6).text_size(11).text_color(t.muted).cursor_pointer()
          .hover((s) => s.bg(t.hover)).focus((s) => s.bg(t.selected))
          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
          .on_click(() => closeOverlay(props().overlay)).child("Close"))))
      .when(rows.length === 0, (el) => el.child(div().px(10).py(8).text_size(12).text_color(t.muted)
        .child("No named terminals. Focus a terminal and press Alt+N to name it.")))
      .child(v_flex().id("roster").max_h(420).overflow_y_scrollbar()
        .children(rows.map((row) => this.row(row, edge, t))))
      .when(this.install === "missing", (el) => el.child(v_flex().px(10).pt(10).pb(4).gap(6)
        .child(div().text_size(12).text_color(t.muted).child("Install the Namedrop binary to send mentions"))
        .child(div().text_size(10).text_color(t.muted).child(releases))
        .child(Button.new("install-namedrop").h(30).px(10).rounded(7).bg(t.raised)
          .text_size(12).text_color(t.foreground).cursor_pointer()
          .hover((s) => s.bg(t.hover)).focus((s) => s.bg(t.selected))
          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
          .on_click((_e, cx) => cx.open_url(releases)).child("Open releases"))));
  }
}
