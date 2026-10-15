import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { NewDriveDialog, defaultLetter } from "./components/NewDriveDialog";
import * as RadixTooltip from "@radix-ui/react-tooltip";
import type { Profile } from "./lib/types";

// Outside Tauri the UI talks to the in-memory preview backend.

describe("App shell", () => {
  it("shows drives from every host and marks preview data", async () => {
    render(<App />);
    expect(await screen.findByRole("article", { name: /H: gpt2/ })).toBeInTheDocument();
    expect(screen.getByRole("article", { name: /X: training-runs/ })).toBeInTheDocument();
    expect(screen.getByText("Preview data")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Drives/ })).toHaveAttribute("aria-current", "page");
  });

  it("navigates with the keyboard", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("heading", { name: "Drives" });
    await user.keyboard("{Control>}2{/Control}");
    expect(await screen.findByRole("heading", { name: "Activity" })).toBeInTheDocument();
    await user.keyboard("{Control>}4{/Control}");
    expect(await screen.findByRole("heading", { name: "Settings" })).toBeInTheDocument();
    await user.keyboard("{Control>}n{/Control}");
    expect(await screen.findByRole("dialog", { name: "New drive" })).toBeInTheDocument();
  });

  it("switches the theme from settings", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: /Settings/ }));
    const group = await screen.findByRole("radiogroup", { name: "Theme" });
    await user.click(within(group).getByRole("radio", { name: /Dark/ }));
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe("dark"));
    // Arrow keys move within the group.
    await user.keyboard("{ArrowRight}");
    await waitFor(() => expect(document.documentElement.dataset.theme).toBeUndefined());
    expect(within(group).getByRole("radio", { name: /System/ })).toHaveAttribute("aria-checked", "true");
  });

  it("shows the signed-in account", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: /^Account/ }));
    expect(await screen.findByText("Alice Martin")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign out" })).toBeInTheDocument();
  });
});

describe("New drive dialog", () => {
  const renderDialog = (onSubmit = vi.fn().mockResolvedValue({}), profiles: Profile[] = []) =>
    render(
      <RadixTooltip.Provider>
        <NewDriveDialog
          open
          onOpenChange={() => {}}
          freeLetters={["E:", "Y:", "Z:"]}
          profiles={profiles}
          account={{ state: "signed_out" }}
          driverInstalled
          onSubmit={onSubmit}
          onSignIn={() => {}}
          onInstallDriver={() => {}}
        />
      </RadixTooltip.Provider>,
    );

  it("fills in the fields from a pasted Hub link and mounts", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn().mockResolvedValue({});
    renderDialog(onSubmit);
    const input = screen.getByLabelText("Repository");
    await user.click(input);
    await user.paste("https://huggingface.co/datasets/HuggingFaceFW/fineweb-edu/tree/main/sample/10BT");
    expect(input).toHaveValue("HuggingFaceFW/fineweb-edu");
    expect(screen.getByRole("radio", { name: "Dataset" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByLabelText("Folder")).toHaveValue("sample/10BT");
    await user.click(screen.getByRole("button", { name: "Mount as Z:" }));
    expect(onSubmit).toHaveBeenCalledWith({
      spec: {
        source_type: "dataset",
        id: "HuggingFaceFW/fineweb-edu",
        subfolder: "sample/10BT",
        revision: "main",
        target: "Z:",
        read_only: true,
      },
      save: true,
      autoMount: false,
    });
  });

  it("validates before mounting and shows engine errors with a next step", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn().mockRejectedValue({ code: "auth_required", message: "model org/private was not found, or it is private or gated" });
    renderDialog(onSubmit);
    await user.click(screen.getByRole("button", { name: "Mount as Z:" }));
    expect(screen.getByText(/Enter a repository id/)).toBeInTheDocument();
    expect(onSubmit).not.toHaveBeenCalled();
    await user.type(screen.getByLabelText("Repository"), "org/private");
    await user.keyboard("{Enter}");
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Sign-in needed");
    expect(within(alert).getByRole("button", { name: "Sign in" })).toBeInTheDocument();
  });

  it("buckets are writable by default and can be made read-only", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn().mockResolvedValue({});
    renderDialog(onSubmit);
    await user.click(screen.getByRole("radio", { name: "Bucket" }));
    await user.type(screen.getByLabelText("Bucket"), "alice/runs");
    await user.click(screen.getByRole("switch", { name: "Allow changes" }));
    await user.click(screen.getByRole("button", { name: "Mount as Z:" }));
    expect(onSubmit.mock.calls[0]![0].spec).toMatchObject({ source_type: "bucket", read_only: true, revision: null });
  });
});

describe("defaultLetter", () => {
  it("prefers the highest free letter not saved for another drive", () => {
    const p = { spec: { target: "Z:" } } as Profile;
    expect(defaultLetter(["E:", "Y:", "Z:"], [])).toBe("Z:");
    expect(defaultLetter(["E:", "Y:", "Z:"], [p])).toBe("Y:");
    expect(defaultLetter(["Z:"], [p])).toBe("Z:");
    expect(defaultLetter([], [])).toBe("");
  });
});
