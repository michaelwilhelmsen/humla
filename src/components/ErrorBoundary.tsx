import React from "react";

type Props = { children: React.ReactNode };
type State = { error: Error | null; info: React.ErrorInfo | null };

export class ErrorBoundary extends React.Component<Props, State> {
  state: State = { error: null, info: null };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    this.setState({ error, info });
    console.error("[ErrorBoundary]", error, info);
  }

  reset = () => this.setState({ error: null, info: null });

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="h-full w-full overflow-auto p-8 font-mono text-sm">
        <h1 className="text-xl font-bold mb-2 text-red-600 dark:text-red-400">
          Something crashed
        </h1>
        <p className="mb-4 text-[var(--color-text-muted)]">
          The UI hit an error. Copy this and share it so it can be fixed.
        </p>
        <pre className="whitespace-pre-wrap p-4 rounded-md border border-[var(--color-line)] bg-[var(--color-surface-raised)] text-[var(--color-text)]">
{this.state.error.name}: {this.state.error.message}
{"\n\n"}
{this.state.error.stack}
{this.state.info?.componentStack ? "\n\nComponent stack:" + this.state.info.componentStack : ""}
        </pre>
        <button
          onClick={this.reset}
          className="mt-4 px-3 py-1.5 rounded-md bg-[var(--color-surface)] border border-[var(--color-line)] hover:bg-[var(--color-pill-hover)]"
        >
          Try again
        </button>
      </div>
    );
  }
}
