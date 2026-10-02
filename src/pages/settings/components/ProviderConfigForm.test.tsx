import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ProviderConfigForm } from "./ProviderConfigForm";

describe("ProviderConfigForm", () => {
  it("selects 60db for either default or per-language configuration", async () => {
    const onChange = vi.fn();
    const view = render(<ProviderConfigForm value={{ provider: "openai", model: "whisper-1" }} onChange={onChange} localModels={[]} filterLocalToLanguage="en" />);
    await userEvent.click(screen.getByRole("combobox", { name: "OpenAI" }));
    await userEvent.click(await screen.findByRole("option", { name: "60db" }));
    expect(onChange).toHaveBeenCalledWith({ provider: "sixtydb", model: "60db-stt-v01" });
    view.rerender(<ProviderConfigForm value={{ provider: "sixtydb", model: "60db-stt-v01" }} onChange={onChange} localModels={[]} />);
    expect(screen.getByText("60db STT v01")).toBeInTheDocument();
    expect(screen.queryByText("Quality")).not.toBeInTheDocument();
  });
});
