/**
 * A fake Tauri IPC boundary for tests.
 *
 * Tests drive the **real** `src/api/invoke.ts` rather than a hand-rolled replacement of
 * it. That matters: `invoke.ts` holds the camelCase-argument-to-Rust-command-name mapping
 * and the response typing, and mocking the module away would leave the one layer that
 * silently rots untested. `mockIPC` intercepts at `window.__TAURI_INTERNALS__`, below
 * that mapping, so every call still goes through it.
 *
 * On top of Tauri's own `@tauri-apps/api/mocks` this adds a per-test command table keyed by
 * Rust command name, and an event bridge that can both count subscriptions and deliver an
 * event to them.
 */

import { mockIPC } from "@tauri-apps/api/mocks";
import type { Annotation } from "../api/types";

/** A handler for one command. Receives the Rust-side (snake_case) argument object. */
export type CommandHandler = (args: Record<string, unknown>) => unknown;

export interface FakeBackend {
  /** Registers the handler for a command name, replacing any previous one. */
  on(command: string, handler: CommandHandler): void;
  /** Every call made, in order, with the command name and its arguments. */
  readonly calls: { command: string; args: Record<string, unknown> }[];
  /** Calls recorded for one command, in order. */
  callsTo(command: string): Record<string, unknown>[];
  /** How many event subscriptions are currently registered. */
  eventListenCount(): number;
  /** Emits a Tauri event to whatever is currently listening for it. */
  emitEvent(event: string, payload: unknown): void;
  /** Restores the real (absent) internals; call from `afterEach`. */
  teardown(): void;
}

let sequence = 0;

/**
 * The IPC internals Tauri installs on `window`. Not part of the API's public typings, so
 * reached through one cast here rather than sprinkled through the tests.
 */
interface TauriInternals {
  runCallback(id: number, data: unknown): void;
}

const tauriInternals = () =>
  (window as unknown as { __TAURI_INTERNALS__: TauriInternals }).__TAURI_INTERNALS__;

/**
 * Builds a deterministic annotation row. Ids ascend so a test can assert on ordering
 * without depending on time or randomness.
 */
export function makeAnnotation(overrides: Partial<Annotation> = {}): Annotation {
  sequence += 1;
  const id = `ann-${sequence}`;
  return {
    id,
    documentId: "doc-1",
    page: 0,
    kind: "note",
    data: JSON.stringify({ v: 1, point: [10, 10] }),
    text: null,
    createdAt: 1_000,
    modifiedAt: 1_000,
    ...overrides,
  };
}

/**
 * Deep-copies a value the way a real IPC round trip does.
 *
 * This is not incidental tidiness. Tauri serialises every command's arguments and results
 * across the webview boundary, so no object is ever shared between the frontend and the
 * backend. A fake that hands back the same reference it stored lets a caller mutate a
 * "persisted" row by holding it -- for instance, an undo entry closing over a row whose
 * text it already changed. That is a test artefact, not a product bug, and copying at the
 * boundary is what stops the two being confused.
 */
function overTheWire<T>(value: T): T {
  return value === undefined ? value : JSON.parse(JSON.stringify(value));
}

/**
 * Installs the fake and returns a handle. Commands with no registered handler reject, so
 * an unexpected call surfaces as a failure instead of a silent `undefined` flowing into a
 * component.
 */
export function installFakeBackend(): FakeBackend {
  const handlers = new Map<string, CommandHandler>();
  const calls: { command: string; args: Record<string, unknown> }[] = [];
  /** Live event subscriptions: what `listen` registered, keyed by the callback id Tauri handed back. */
  const subscriptions: { event: string; handlerId: number; eventId: number }[] = [];
  let nextEventId = 1;

  mockIPC((command, args) => {
    const record = overTheWire(args) as Record<string, unknown>;
    calls.push({ command, args: record });

    // The event plugin is handled here rather than through `mockIPC`'s `shouldMockEvents`
    // option: that option intercepts these commands inside Tauri's own wrapper, before this
    // callback runs, which would make subscriptions invisible to `eventListenCount`.
    if (command === "plugin:event|listen") {
      const eventId = nextEventId;
      nextEventId += 1;
      subscriptions.push({
        event: record.event as string,
        handlerId: record.handler as number,
        eventId,
      });
      return eventId;
    }
    if (command === "plugin:event|unlisten") {
      const index = subscriptions.findIndex(
        (entry) => entry.eventId === (record.eventId as number),
      );
      if (index !== -1) subscriptions.splice(index, 1);
      return null;
    }

    const handler = handlers.get(command);
    if (!handler) {
      return Promise.reject({
        code: "UNMOCKED",
        message: `no handler registered for command: ${command}`,
      });
    }
    // Copied on the way out for the same reason as on the way in.
    return Promise.resolve(handler(record)).then(overTheWire);
  });

  // `_unlisten` calls this before its IPC round trip. Without it, every component unmount
  // throws -- and since unmount happens during cleanup, the error surfaces as an unhandled
  // rejection that can fail an unrelated test.
  (window as unknown as { __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener?: unknown } }).__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener ??= () => {};

  return {
    on(command, handler) {
      handlers.set(command, handler);
    },
    get calls() {
      return calls;
    },
    callsTo(command) {
      return calls.filter((entry) => entry.command === command).map((entry) => entry.args);
    },
    eventListenCount() {
      return subscriptions.length;
    },
    emitEvent(event, payload) {
      // Dispatched straight to the registered callback, which is exactly what the Rust side
      // does when it emits: `runCallback(handlerId, event)`. Going through the API's `emit`
      // instead would need the whole event plugin mocked just to reach the same place.
      for (const subscription of [...subscriptions]) {
        if (subscription.event !== event) continue;
        tauriInternals().runCallback(subscription.handlerId, {
          event,
          id: subscription.eventId,
          payload,
        });
      }
    },
    teardown() {
      // Deliberately NOT `clearMocks()`. React Testing Library unmounts components in an
      // `afterEach` registered after this file's, so teardown runs while the tree is still
      // mounted: deleting `invoke` here would make every unmount throw "invoke is not a
      // function" as an unhandled rejection. `mockIPC` reinstalls the internals at the start
      // of the next test, so there is nothing to undo eagerly.
      handlers.clear();
      calls.length = 0;
      subscriptions.length = 0;
      nextEventId = 1;
    },
  };
}
