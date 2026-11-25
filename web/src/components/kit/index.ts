// Presentational building blocks shared across the desktop app. Each takes
// plain props and fetches nothing; see the file for its rules.
export { SessionCard, SessionStatus } from "./SessionCard";
export { Sheet } from "./Sheet";
export { Stepper } from "./Stepper";
export { DataTable, type Column } from "./DataTable";
export { Combobox, type ComboOption } from "./Combobox";
export { Fingerprint, groupFingerprint, groupMatchCode, spellFingerprint, spellMatchCode } from "./Fingerprint";
export { AvatarStack, PresenceDot, presenceColor, type Presence, type StackPerson } from "./Presence";
export { InboxItem } from "./InboxItem";
export { StatusBar, StatusBarItem } from "./StatusBar";
export { Countdown, formatRemaining } from "./Countdown";
export { VirtualList, visibleRange } from "./VirtualList";
