import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Section, SelectField } from "@/components/ui";
import { InfoField } from "@/components/settings/shared";
import { toast, errMsg } from "@/components/Toast";
import { proposalArchiveDays, setProposalArchiveDays } from "@/lib/ipc";

/// The choices offered (days; 0 = off). A workspace set elsewhere (lt, an
/// older choice) to another number still shows it.
export const ARCHIVE_DAY_CHOICES = [0, 1, 3, 7, 14, 30, 90];

export function archiveDaysLabel(d: number): string {
  if (d === 0) return "Off";
  return d === 1 ? "After 1 day" : `After ${d} days`;
}

/// Settings → Review: when settled proposals (applied, rejected, replaced by
/// a revision) leave the inbox for Archived proposals. A workspace setting on
/// the config log; an Admin changes it. Nothing is deleted and no device
/// writes anything when a proposal ages out.
export function ReviewSection() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["proposal-archive-days"], queryFn: proposalArchiveDays });
  const save = useMutation({
    mutationFn: (days: number) => setProposalArchiveDays(days),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["proposal-archive-days"] });
      void qc.invalidateQueries({ queryKey: ["proposals"] });
      void qc.invalidateQueries({ queryKey: ["archived-proposals"] });
    },
    onError: (e) => toast.error(`Couldn't save: ${errMsg(e)}`),
  });
  if (q.isError) {
    return (
      <Section title="Review">
        <p role="alert" className="text-sm" style={{ color: "var(--ligus-danger-ink)" }}>
          Couldn't load the setting: {errMsg(q.error)}
        </p>
      </Section>
    );
  }
  if (!q.data) return null;
  const days = q.data.days ?? 0;
  const choices = ARCHIVE_DAY_CHOICES.includes(days) ? ARCHIVE_DAY_CHOICES : [...ARCHIVE_DAY_CHOICES, days].sort((a, b) => a - b);
  return (
    <Section title="Review">
      <InfoField
        label="Auto-archive settled proposals"
        info="Applied, rejected and replaced proposals move to Archived proposals after this long, for everyone in the workspace. Nothing waiting on a decision is archived. An Admin changes it."
      >
        <SelectField
          value={String(days)}
          onChange={(v) => save.mutate(Number(v))}
          disabled={!q.data.canEdit || save.isPending}
          ariaLabel="Auto-archive settled proposals"
        >
          {choices.map((d) => (
            <option key={d} value={d}>
              {archiveDaysLabel(d)}
            </option>
          ))}
        </SelectField>
      </InfoField>
    </Section>
  );
}
