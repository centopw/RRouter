import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { Button, ErrorNote, Field, Input, Modal, Select } from "../../components/ui";
import { Checkbox } from "../../components/atoms";
import { CAPABILITY_KEYS, CAPABILITY_LABELS } from "../../components/capability-chips";
import { api, type Capabilities, type Model, type ModelProvider } from "../../lib/api";
import { numeric } from "./rows";

/** The one free-text model field in the app. Mounted only while open, so typing in it
 *  never re-renders the registry table behind the dialog. */
export function CustomModelModal({
  providers,
  defaultProvider,
  initial,
  onClose,
  onSaved,
}: {
  providers: ModelProvider[] | undefined;
  defaultProvider: string;
  /** Present when editing an existing row: it becomes a custom override of that ref. */
  initial?: Model;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [form, setForm] = useState({
    provider: initial?.provider ?? (defaultProvider || providers?.[0]?.id || ""),
    id: initial?.id ?? "",
    display: initial?.display ?? "",
    context: initial?.context ? String(initial.context) : "",
    maxOutput: initial?.max_output ? String(initial.max_output) : "",
    priceIn: initial?.cost ? String(initial.cost.input) : "",
    priceOut: initial?.cost ? String(initial.cost.output) : "",
  });
  const [caps, setCaps] = useState<Partial<Record<keyof Capabilities, boolean>>>(
    initial ? { ...initial.capabilities } : {},
  );

  const create = useMutation({
    mutationFn: () =>
      api.post<Model>("/api/models/custom", {
        provider: form.provider,
        id: form.id.trim(),
        display: form.display.trim() || null,
        context: numeric(form.context),
        max_output: numeric(form.maxOutput),
        cost:
          form.priceIn.trim() !== "" || form.priceOut.trim() !== ""
            ? {
                input: Number(form.priceIn) || 0,
                output: Number(form.priceOut) || 0,
              }
            : null,
        capabilities: Object.fromEntries(CAPABILITY_KEYS.map((key) => [key, caps[key] === true])),
      }),
    onSuccess: () => {
      onSaved();
      onClose();
    },
  });

  const ready = form.provider !== "" && form.id.trim() !== "" && !create.isPending;

  return (
    <Modal
      title={initial ? "edit model" : "add custom model"}
      description={initial ? `${initial.provider}/${initial.id} — saves an operator override for this model.` : undefined}
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>cancel</Button>
          <Button variant="primary" disabled={!ready} onClick={() => create.mutate()}>
            {create.isPending ? "saving…" : initial ? "save changes" : "add model"}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="grid grid-cols-2 gap-3">
          <Field label="provider">
            <Select
              ariaLabel="custom model provider"
              value={form.provider}
              onChange={(v) => setForm((f) => ({ ...f, provider: v }))}
              disabled={Boolean(initial)}
            >
              {(providers ?? []).map((p) => (
                <option key={p.id} value={p.id}>
                  {p.display}
                </option>
              ))}
              {/* Editing a row whose provider lost its connection still needs a selected
                  option; the value never changes while this select is disabled. */}
              {form.provider !== "" && !(providers ?? []).some((p) => p.id === form.provider) ? (
                <option value={form.provider}>{form.provider} (not connected)</option>
              ) : null}
            </Select>
          </Field>
          <Field label="model id" hint="the only free-text model field">
            <Input
              value={form.id}
              onChange={(v) => setForm((f) => ({ ...f, id: v }))}
              placeholder="my-model"
              ariaLabel="custom model id"
              disabled={Boolean(initial)}
            />
          </Field>
        </div>
        <Field label="display">
          <Input
            value={form.display}
            onChange={(v) => setForm((f) => ({ ...f, display: v }))}
            placeholder="My Model"
            ariaLabel="custom model display name"
          />
        </Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="context" hint="tokens">
            <Input
              type="number"
              value={form.context}
              onChange={(v) => setForm((f) => ({ ...f, context: v }))}
              placeholder="128000"
              ariaLabel="custom model context window"
            />
          </Field>
          <Field label="max out" hint="tokens">
            <Input
              type="number"
              value={form.maxOutput}
              onChange={(v) => setForm((f) => ({ ...f, maxOutput: v }))}
              placeholder="8192"
              ariaLabel="custom model max output"
            />
          </Field>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <Field label="price in /1M" hint="USD per million tokens; empty = unpublished">
            <Input
              type="number"
              value={form.priceIn}
              onChange={(v) => setForm((f) => ({ ...f, priceIn: v }))}
              placeholder="1.00"
              ariaLabel="custom model input price"
            />
          </Field>
          <Field label="price out /1M" hint="USD per million tokens">
            <Input
              type="number"
              value={form.priceOut}
              onChange={(v) => setForm((f) => ({ ...f, priceOut: v }))}
              placeholder="4.00"
              ariaLabel="custom model output price"
            />
          </Field>
        </div>
        <div className="flex min-w-0 flex-col gap-1.5">
          <span className="text-small font-semibold text-ink-soft">capabilities</span>
          <div className="grid grid-cols-3 gap-x-3 gap-y-1.5">
            {CAPABILITY_KEYS.map((key) => (
              <Checkbox
                key={key}
                checked={caps[key] === true}
                onChange={(next) => setCaps((c) => ({ ...c, [key]: next }))}
                label={CAPABILITY_LABELS[key].chip}
              />
            ))}
          </div>
        </div>
        {create.isError ? <ErrorNote error={create.error} /> : null}
      </div>
    </Modal>
  );
}
