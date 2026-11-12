import {
  type ReactNode,
  type RefObject,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import type { AddressId } from "../model.ts";
import {
  agentKey,
  audible,
  SOUND_NAMES,
  type SoundChange,
  type SoundId,
  soundFor,
  TONES,
  type Tone,
} from "../sounds.ts";
import { Icon } from "./Icons.tsx";
import { type SoundAccess, useSoundAccess } from "./useSounds.ts";

// `null` is the agent's Default row: it follows the Default sound.
type Choice = SoundId | null;
type Failure = Exclude<Awaited<ReturnType<SoundAccess["change"]>>, "saved">;
const FAILURES: Readonly<Record<Failure, string>> = {
  full: "Couldn't save that sound. Too many agents have their own sound.",
  failed: "Couldn't save that. Try again.",
};
const CHOICES: readonly SoundId[] = [...TONES, "none"];

function useDismiss(details: RefObject<HTMLDetailsElement | null>) {
  useEffect(() => {
    const outside = (event: PointerEvent) => {
      const node = details.current;
      if (node?.open && !node.contains(event.target as Node)) node.open = false;
    };
    const onEscape = (event: KeyboardEvent) => {
      const node = details.current;
      if (
        event.key !== "Escape" ||
        !node?.open ||
        !node.contains(document.activeElement)
      )
        return;
      event.preventDefault();
      event.stopPropagation();
      node.open = false;
      node.querySelector("summary")?.focus();
    };
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("keydown", onEscape, true);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("keydown", onEscape, true);
    };
  }, [details]);
}

function SoundMenu({
  sounds,
  className,
  summary,
  title,
  legend,
  value,
  choices,
  name,
  change,
  children,
}: {
  sounds: SoundAccess;
  className: string;
  summary: ReactNode;
  title?: string;
  legend: string;
  value: Choice;
  choices: readonly Choice[];
  name(choice: Choice): string;
  change(choice: Choice): SoundChange;
  children?: (save: (change: SoundChange) => void) => ReactNode;
}) {
  const details = useRef<HTMLDetailsElement>(null);
  const group = useId();
  const [failed, setFailed] = useState<Failure | null>(null);
  useDismiss(details);
  const { settings } = sounds;
  const save = (change: SoundChange) => {
    setFailed(null);
    void sounds
      .change(change)
      .then((saved) => setFailed(saved === "saved" ? null : saved));
  };
  const toneOf = (choice: Choice): Tone | null => {
    const sound = choice ?? settings?.default ?? "none";
    return sound === "none" ? null : sound;
  };
  return (
    <details
      className={`sound-picker ${className}`}
      ref={details}
      onToggle={(event) => {
        if (event.currentTarget.open) sounds.refresh();
        else setFailed(null);
      }}
    >
      <summary title={title}>{summary}</summary>
      <div className="sound-menu">
        {settings === null ? (
          <p className="sound-note">Sounds are unavailable right now.</p>
        ) : (
          <>
            {children?.(save)}
            <fieldset>
              <legend>{legend}</legend>
              {choices.map((choice) => {
                const tone = toneOf(choice);
                return (
                  <div className="sound-row" key={choice ?? "default"}>
                    <label>
                      <input
                        type="radio"
                        name={group}
                        checked={value === choice}
                        onChange={() => save(change(choice))}
                      />
                      {name(choice)}
                    </label>
                    {tone !== null && (
                      <button
                        type="button"
                        className="sound-preview"
                        aria-label={`Play ${name(choice)}`}
                        title={`Play ${name(choice)}`}
                        onClick={() => sounds.preview(tone)}
                      >
                        <Icon name="play" />
                      </button>
                    )}
                  </div>
                );
              })}
            </fieldset>
            {failed !== null && (
              <p className="sound-note" role="alert">
                {FAILURES[failed]}
              </p>
            )}
          </>
        )}
      </div>
    </details>
  );
}

export function SoundPicker({
  agent,
  alias,
}: {
  agent: AddressId;
  alias: string;
}) {
  const sounds = useSoundAccess();
  const key = agentKey(agent);
  if (sounds === null || key === null) return null;
  const { settings } = sounds;
  const value = settings?.agents.get(key) ?? null;
  const fallback = SOUND_NAMES[settings?.default ?? "none"];
  const name = (choice: Choice) =>
    choice === null ? `Default (${fallback})` : SOUND_NAMES[choice];
  const label = value === null ? "Default" : SOUND_NAMES[value];
  return (
    <SoundMenu
      sounds={sounds}
      className="agent-sound"
      summary={
        <>
          <Icon
            name={soundFor(settings, key) === null ? "speakerOff" : "speaker"}
          />
          <span className="sr-only">Sound for {alias}: </span>
          <span className="sound-label">{label}</span>
        </>
      }
      title={`Sound for ${alias}: ${label}`}
      legend={`Sound for ${alias}`}
      value={value}
      choices={[null, ...CHOICES]}
      name={name}
      change={(sound) => ({ kind: "agent", agent: key, sound })}
    />
  );
}

export function SoundSettingsControl() {
  const sounds = useSoundAccess();
  if (sounds === null) return null;
  const { settings } = sounds;
  const muted = settings?.muted ?? false;
  return (
    <SoundMenu
      sounds={sounds}
      className="sound-settings"
      summary={
        <>
          <Icon name={muted ? "speakerOff" : "speaker"} />
          {muted ? "Muted" : "Sounds"}
        </>
      }
      legend="Default sound"
      value={settings?.default ?? null}
      choices={CHOICES}
      name={(choice) => SOUND_NAMES[choice ?? "none"]}
      change={(sound) => ({ kind: "default", sound: sound ?? "none" })}
    >
      {(save) => (
        <label className="sound-mute">
          <input
            type="checkbox"
            checked={muted}
            onChange={(event) =>
              save({ kind: "muted", muted: event.target.checked })
            }
          />
          Mute all sounds
        </label>
      )}
    </SoundMenu>
  );
}

export function SoundChip() {
  const sounds = useSoundAccess();
  if (
    sounds === null ||
    sounds.player !== "locked" ||
    !audible(sounds.settings)
  )
    return null;
  return (
    <button type="button" className="sound-chip" onClick={sounds.unlock}>
      <Icon name="speaker" />
      Click to turn on sounds
    </button>
  );
}
