import { View, div } from "gpui-kit";
import { h_flex, Button } from "gpui-base";
import { home, focused, terminals, exec, hc, openOverlay, closeOverlay, props, theme, run } from "herdr";
import { launchPopup } from "./launcher.js";

const popup = "doctor";

export default class PopupLauncher extends View {
  init(_props, cx) {
    if (props()?.part === "bar") return;
    this.error = null;
    cx.spawn(async (cx) => {
      const result = await launchPopup({ home, focused, terminals, exec, hc, openOverlay, props: props() ?? {} });
      if (result.error) {
        this.error = result.error;
        cx.notify();
      } else if (props()?.overlay) closeOverlay(props().overlay);
    });
  }

  render() {
    const t = theme();
    if (props()?.part === "bar") return h_flex().h(28).px(8).items_center().bg(t.raised)
      .child(div().flex_1().text_size(12).child("Namedrop " + popup))
      .child(Button.new("close")
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click(() => run("terminal.kill", { terminal: props().terminal })).child("×"));
    return div().p(12).text_color(t.foreground).child(this.error ?? "Opening Namedrop…");
  }
}
