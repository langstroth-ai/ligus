import { Icon } from "@/lib/icons";

/// Three comb cells: the Flow pane's glyph.
export const IconComb = ({ size }: { size?: number }) => (
  <Icon size={size}>
    <path d="M7 3.5l3 1.75v3.5L7 10.5 4 8.75v-3.5z" />
    <path d="M17 3.5l3 1.75v3.5l-3 1.75-3-1.75v-3.5z" />
    <path d="M12 12.5l3 1.75v3.5l-3 1.75-3-1.75v-3.5z" />
  </Icon>
);
