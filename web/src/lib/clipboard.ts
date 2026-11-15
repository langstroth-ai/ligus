// Copy text that is still being fetched. WebKit (the macOS and Linux
// webviews) only lets a page write to the clipboard during the click that
// asked for it; awaiting an IPC call first (an invite that may wait on a
// passkey prompt) loses that permission and the write fails with
// NotAllowedError. Handing the clipboard a ClipboardItem whose content is a
// promise starts the write inside the click and fills it in when the text
// arrives. Call this synchronously from the click handler, before any await.
//
// Resolves with the copied text. If producing the text fails, rejects with
// that error rather than the clipboard's generic one.
export function copyWhenReady(text: Promise<string>): Promise<string> {
  const clip = navigator.clipboard;
  if (typeof ClipboardItem !== "undefined" && clip?.write) {
    const item = new ClipboardItem({
      "text/plain": text.then((t) => new Blob([t], { type: "text/plain" })),
    });
    return Promise.all([text, clip.write([item]).catch((e: unknown) => text.then(() => Promise.reject(e)))]).then(
      ([t]) => t,
    );
  }
  return text.then((t) => clip.writeText(t).then(() => t));
}
