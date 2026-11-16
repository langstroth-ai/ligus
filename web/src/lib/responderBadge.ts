/// The badge on a primary `@hive` turn: which device answered it. Your own
/// turns are labelled by place ("This device", "Your other device"), never
/// by your display name: a fresh desktop is called "You", which read as a
/// "YOU" badge on the agent's own message. Someone else's turn names them.
export function responderBadge(
  name: string | null | undefined,
  device: string | null | undefined,
): { label: string; title: string } | null {
  if (device === "this") return { label: "This device", title: "Answered by @hive on this device" };
  if (device === "mine") return { label: "Your other device", title: "Answered by @hive on another of your devices" };
  const n = name?.trim();
  if (!n) return null;
  return { label: `${n}'s device`, title: `Answered by ${n}'s @hive` };
}
