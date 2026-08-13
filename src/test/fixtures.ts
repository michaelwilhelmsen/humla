import type { Note } from "../lib/ipc";

/**
 * A minimal live Note. Only the fields a test actually asserts on are worth
 * passing; everything else takes an inert default so a new column on `Note`
 * doesn't break every fixture.
 */
export function makeNote(overrides: Partial<Note> & { id: string }): Note {
  return {
    title: overrides.id,
    body: "",
    transcript: "",
    summary: "",
    audio_path: null,
    summary_preset: "meeting",
    folder_id: null,
    language: "",
    summary_provider: "",
    expected_speakers: null,
    detected_language: null,
    created_at: 0,
    updated_at: 0,
    owner: "",
    workspace_id: "",
    ...overrides,
  };
}
