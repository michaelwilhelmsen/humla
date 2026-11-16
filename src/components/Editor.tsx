import { useEditor, useEditorState, EditorContent, type Editor } from "@tiptap/react";
import { BubbleMenu } from "@tiptap/react/menus";
import StarterKit from "@tiptap/starter-kit";
import { Placeholder } from "@tiptap/extensions";
import { useEffect, useRef } from "react";
import { SlashCommand, makeSlashSuggestion } from "../lib/slash";
import { cn } from "../lib/cn";

type Props = {
  initialHTML: string;
  onChange: (html: string) => void;
  placeholder?: string;
  editable?: boolean;
};

export function NoteEditor({
  initialHTML,
  onChange,
  placeholder = "Write your notes here…",
  editable = true,
}: Props) {
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  const editor = useEditor({
    extensions: [
      // Link would make a click navigate the app's own webview. TrailingNode
      // appends a paragraph to a body ending in a heading or list on the first
      // transaction, which saves and syncs an edit nobody made.
      StarterKit.configure({
        heading: { levels: [1, 2, 3, 4] },
        link: false,
        underline: false,
        trailingNode: false,
      }),
      Placeholder.configure({
        placeholder,
        emptyNodeClass:
          "before:content-[attr(data-placeholder)] before:text-[var(--color-text-muted)] before:float-left before:h-0 before:pointer-events-none",
      }),
      SlashCommand.configure({
        suggestion: makeSlashSuggestion(),
      }),
    ],
    content: initialHTML || "",
    editable,
    editorProps: {
      attributes: {
        class: "prose-note focus:outline-none",
      },
    },
    onUpdate: ({ editor }) => {
      onChangeRef.current(editor.getHTML());
    },
  });

  // Toggle editability live (e.g. when the active workspace role changes to/from
  // read-only viewer).
  useEffect(() => {
    if (editor && editor.isEditable !== editable) editor.setEditable(editable, false);
  }, [editor, editable]);

  // Re-sync content if the note id changes (parent passes a new initialHTML).
  useEffect(() => {
    if (!editor) return;
    const current = editor.getHTML();
    if ((initialHTML || "") !== current) {
      editor.commands.setContent(initialHTML || "", { emitUpdate: false });
    }
  }, [editor, initialHTML]);

  if (!editor) return null;

  return (
    <>
      <BubbleMenu
        editor={editor}
        options={BUBBLE_OPTIONS}
        shouldShow={showBubble}
        className="z-50"
      >
        <div className="bubble-menu flex items-center gap-0.5 p-1 rounded-md bg-[var(--color-surface)] border border-[var(--color-line)] shadow-md text-[var(--color-text)]">
          <BubbleBtn editor={editor} action="bold" label="B" className="font-bold" />
          <BubbleBtn editor={editor} action="italic" label="I" className="italic" />
          <BubbleBtn editor={editor} action="strike" label="S" className="line-through" />
          <BubbleBtn editor={editor} action="code" label="</>" className="font-mono text-xs" />
          <span className="w-px h-4 bg-[var(--color-line)] mx-0.5" />
          <BubbleBtn editor={editor} action="heading1" label="H1" />
          <BubbleBtn editor={editor} action="heading2" label="H2" />
          <BubbleBtn editor={editor} action="bulletList" label="•" />
          <BubbleBtn editor={editor} action="orderedList" label="1." />
        </div>
      </BubbleMenu>

      <EditorContent editor={editor} />
    </>
  );
}

// Module-level so a re-render doesn't hand BubbleMenu new values, which it
// answers with a transaction.
const BUBBLE_OPTIONS = { placement: "top", offset: 8 } as const;
const showBubble = ({ editor, from, to }: { editor: Editor; from: number; to: number }) =>
  from !== to && !editor.isActive("horizontalRule");

type Action = "bold" | "italic" | "strike" | "code" | "heading1" | "heading2" | "bulletList" | "orderedList";

function BubbleBtn({
  editor,
  action,
  label,
  className,
}: {
  editor: Editor;
  action: Action;
  label: string;
  className?: string;
}) {
  const isActive = useEditorState({
    editor,
    selector: ({ editor }) => isActionActive(editor, action),
  });

  function onClick() {
    const c = editor.chain().focus();
    switch (action) {
      case "bold": c.toggleBold().run(); break;
      case "italic": c.toggleItalic().run(); break;
      case "strike": c.toggleStrike().run(); break;
      case "code": c.toggleCode().run(); break;
      case "heading1": c.toggleHeading({ level: 1 }).run(); break;
      case "heading2": c.toggleHeading({ level: 2 }).run(); break;
      case "bulletList": c.toggleBulletList().run(); break;
      case "orderedList": c.toggleOrderedList().run(); break;
    }
  }

  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "px-2 py-1 rounded text-sm min-w-[28px]",
        isActive
          ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
          : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)]",
        className
      )}
    >
      {label}
    </button>
  );
}

function isActionActive(editor: Editor, action: Action): boolean {
  switch (action) {
    case "bold": return editor.isActive("bold");
    case "italic": return editor.isActive("italic");
    case "strike": return editor.isActive("strike");
    case "code": return editor.isActive("code");
    case "heading1": return editor.isActive("heading", { level: 1 });
    case "heading2": return editor.isActive("heading", { level: 2 });
    case "bulletList": return editor.isActive("bulletList");
    case "orderedList": return editor.isActive("orderedList");
  }
}
