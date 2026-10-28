use call::ActiveCall;
use gpui::App;

pub fn toggle_mute(cx: &mut App) {
    let call = ActiveCall::global(cx).read(cx);
    if let Some(room) = call.room().cloned() {
        room.update(cx, |room, cx| room.toggle_mute(cx));
    }
}

pub fn toggle_deafen(cx: &mut App) {
    if let Some(room) = ActiveCall::global(cx).read(cx).room().cloned() {
        room.update(cx, |room, cx| room.toggle_deafen(cx));
    }
}
