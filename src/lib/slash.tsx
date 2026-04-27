import { Extension, ReactRenderer, type Editor, type Range } from "@tiptap/react";
import Suggestion from "@tiptap/suggestion";
import tippy, { type Instance as TippyInstance } from "tippy.js";
import { useEffect, useImperativeHandle, useState, forwardRef } from "react";

export type SlashItem = {
  title: string;
  hint?: string;
  command: (args: { editor: Editor; range: Range }) => void;
};

export const SLASH_ITEMS: SlashItem[] = [
  {
    title: "Heading 1",
    hint: "# ",
    command: ({ editor, range }) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 1 }).run(),
  },
  {
    title: "Heading 2",
    hint: "## ",
    command: ({ editor, range }) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 2 }).run(),
  },
  {
    title: "Heading 3",
    hint: "### ",
    command: ({ editor, range }) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 3 }).run(),
  },
  {
    title: "Heading 4",
    hint: "#### ",
    command: ({ editor, range }) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 4 }).run(),
  },
  {
    title: "Paragraph",
    hint: "Plain text",
    command: ({ editor, range }) =>
      editor.chain().focus().deleteRange(range).setParagraph().run(),
  },
  {
    title: "Bullet list",
    hint: "- item",
    command: ({ editor, range }) =>
      editor.chain().focus().deleteRange(range).toggleBulletList().run(),
  },
  {
    title: "Numbered list",
    hint: "1. item",
    command: ({ editor, range }) =>
      editor.chain().focus().deleteRange(range).toggleOrderedList().run(),
  },
  {
    title: "Quote",
    hint: "> quote",
    command: ({ editor, range }) =>
      editor.chain().focus().deleteRange(range).toggleBlockquote().run(),
  },
  {
    title: "Divider",
    hint: "---",
    command: ({ editor, range }) =>
      editor.chain().focus().deleteRange(range).setHorizontalRule().run(),
  },
];

type ListProps = {
  items: SlashItem[];
  command: (item: SlashItem) => void;
};

type ListHandle = {
  onKeyDown: (props: { event: KeyboardEvent }) => boolean;
};

const SlashList = forwardRef<ListHandle, ListProps>(({ items, command }, ref) => {
  const [selected, setSelected] = useState(0);

  useEffect(() => setSelected(0), [items]);

  useImperativeHandle(ref, () => ({
    onKeyDown: ({ event }) => {
      if (event.key === "ArrowDown") {
        setSelected((s) => (s + 1) % Math.max(items.length, 1));
        return true;
      }
      if (event.key === "ArrowUp") {
        setSelected((s) => (s - 1 + items.length) % Math.max(items.length, 1));
        return true;
      }
      if (event.key === "Enter") {
        const item = items[selected];
        if (item) command(item);
        return true;
      }
      return false;
    },
  }));

  if (items.length === 0) {
    return (
      <div className="slash-menu px-3 py-2 text-sm text-[var(--color-text-muted)]">
        No matches
      </div>
    );
  }

  return (
    <div className="slash-menu rounded-lg bg-[var(--color-surface)] border border-[var(--color-line)] shadow-lg overflow-hidden min-w-[220px] py-1">
      {items.map((item, i) => (
        <button
          key={item.title}
          onClick={() => command(item)}
          onMouseEnter={() => setSelected(i)}
          className={
            "w-full text-left flex items-center justify-between gap-3 px-3 py-1.5 text-sm " +
            (i === selected
              ? "bg-[var(--color-pill-hover)]"
              : "hover:bg-[var(--color-pill-hover)]")
          }
        >
          <span>{item.title}</span>
          {item.hint && (
            <span className="text-xs text-[var(--color-text-muted)] font-mono">
              {item.hint}
            </span>
          )}
        </button>
      ))}
    </div>
  );
});

SlashList.displayName = "SlashList";

export const SlashCommand = Extension.create({
  name: "slashCommand",
  addOptions() {
    return {
      suggestion: {
        char: "/",
        startOfLine: false,
        command: ({ editor, range, props }: { editor: Editor; range: Range; props: SlashItem }) => {
          props.command({ editor, range });
        },
      },
    };
  },
  addProseMirrorPlugins() {
    return [
      Suggestion({
        editor: this.editor,
        ...this.options.suggestion,
      }),
    ];
  },
});

export function makeSlashSuggestion() {
  return {
    items: ({ query }: { query: string }) => {
      const q = query.toLowerCase();
      return SLASH_ITEMS.filter((i) => i.title.toLowerCase().includes(q));
    },
    render: () => {
      let component: ReactRenderer<ListHandle, ListProps> | null = null;
      let popup: TippyInstance[] | null = null;

      return {
        onStart: (props: any) => {
          component = new ReactRenderer<ListHandle, ListProps>(SlashList, {
            props,
            editor: props.editor,
          });

          if (!props.clientRect) return;

          popup = tippy("body", {
            getReferenceClientRect: props.clientRect,
            appendTo: () => document.body,
            content: component.element,
            showOnCreate: true,
            interactive: true,
            trigger: "manual",
            placement: "bottom-start",
            offset: [0, 8],
            theme: "slash",
          });
        },
        onUpdate(props: any) {
          component?.updateProps(props);
          if (props.clientRect) {
            popup?.[0]?.setProps({ getReferenceClientRect: props.clientRect });
          }
        },
        onKeyDown(props: any) {
          if (props.event.key === "Escape") {
            popup?.[0]?.hide();
            return true;
          }
          return component?.ref?.onKeyDown(props) ?? false;
        },
        onExit() {
          popup?.[0]?.destroy();
          component?.destroy();
          popup = null;
          component = null;
        },
      };
    },
  };
}
