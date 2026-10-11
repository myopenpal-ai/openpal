import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import Library from "./Library";
import { useStore } from "../state/store";
import type { HardwareInfo, ModelInfo, ModelSpec, Settings, SystemReport } from "../lib/types";

const { openModelsFolder, probeDiskSpeed, setModelFavorite } = vi.hoisted(() => ({
  openModelsFolder: vi.fn().mockResolvedValue(undefined),
  probeDiskSpeed: vi.fn().mockResolvedValue({
    bytesPerSec: 2_000_000_000,
    class: "fast",
    estimatedTokensPerSec: 4,
    calibrated: true,
  }),
  setModelFavorite: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../lib/api", () => ({
  api: { openModelsFolder, probeDiskSpeed, setModelFavorite },
}));

function spec(overrides: Partial<ModelSpec> & { id: string }): ModelSpec {
  return {
    name: overrides.id,
    family: "Test",
    publisher: "Test Publisher",
    description: "A test model.",
    strengths: [],
    url: "https://example.com/model.gguf",
    fileName: "model.gguf",
    sizeBytes: 4 * 1024 ** 3,
    quant: "Q4_K_M",
    sha256: null,
    paramsB: 8,
    moe: false,
    contextMax: 8192,
    qualityScore: 50,
    license: "Apache 2.0",
    extraArgs: [],
    hidden: false,
    curated: false,
    source: "catalog",
    localPath: null,
    vision: false,
    taskScores: {},
    ...overrides,
  };
}

function model(
  overrides: Partial<ModelSpec> & { id: string },
  state: Partial<Pick<ModelInfo, "fit" | "favorite" | "status" | "isRecommended">> = {},
): ModelInfo {
  return {
    spec: spec(overrides),
    status: state.status ?? { type: "notInstalled" },
    fit: state.fit ?? "good",
    why: "It should run comfortably.",
    isRecommended: state.isRecommended ?? false,
    favorite: state.favorite ?? false,
  };
}

const INSTALLED: ModelInfo["status"] = {
  type: "installed",
  data: { path: "/models/x.gguf", sizeBytes: 4 * 1024 ** 3 },
};

const HARDWARE: HardwareInfo = {
  chip: "Apple M2",
  isAppleSilicon: true,
  totalRamBytes: 16 * 1024 ** 3,
  availableRamBytes: 12 * 1024 ** 3,
  cpuCores: 8,
  hasMetal: true,
  freeDiskBytes: 500 * 1024 ** 3,
  osVersion: "14.0",
};

/** Two curated picks (so the dial renders) plus a user-added model (so the
 * list toolbar renders — it stays hidden for a stock four-model install). */
const MODELS: ModelInfo[] = [
  model({ id: "gemma-e2b", name: "Gemma 4 E2B", publisher: "Google", curated: true, qualityScore: 30 }),
  model(
    { id: "gemma-e4b", name: "Gemma 4 E4B", publisher: "Google", curated: true, qualityScore: 68 },
    { status: INSTALLED, fit: "perfect" },
  ),
  model({ id: "qwen-hf", name: "Qwen3 8B", publisher: "Alibaba", source: "huggingFace" }),
];

function seed(models: ModelInfo[] = MODELS) {
  useStore.setState({
    models,
    report: { hardware: HARDWARE, recommendation: null } as unknown as SystemReport,
    settings: { autoLoadOnStartup: true, defaultModelId: null } as unknown as Settings,
    engine: { type: "stopped" },
    engineBusy: false,
    downloads: {},
    downloadErrors: {},
    // An empty cloud catalog hides that section, keeping these assertions to
    // the local list.
    cloudModels: [],
    cloudModelsError: null,
  });
}

/** The "All models" rows, excluding the spotlight card above them. */
function modelRows() {
  return within(screen.getByRole("list", { name: "" })).queryAllByRole("listitem");
}

describe("Library", () => {
  beforeEach(() => {
    seed();
    setModelFavorite.mockClear();
    openModelsFolder.mockClear();
  });

  it("renders every model as a list row, not a table", () => {
    const { container } = render(<Library />);
    // The 13-column table this replaced is gone; a table is what forced the
    // horizontal scrolling the rewrite exists to remove.
    expect(container.querySelector("table")).toBeNull();

    const rows = modelRows();
    expect(rows).toHaveLength(3);
    // Default "recommended" order: what's already on disk first, then by fit,
    // then by quality — so the installed E4B leads, and Qwen (qualityScore 50)
    // comes before E2B (30). Not the order the store handed them over in.
    expect(rows.map((r) => r.textContent)).toEqual([
      expect.stringContaining("Gemma 4 E4B"),
      expect.stringContaining("Qwen3 8B"),
      expect.stringContaining("Gemma 4 E2B"),
    ]);
  });

  it("keeps each row's spec detail on the row instead of in off-screen columns", () => {
    render(<Library />);
    const row = modelRows().find((r) => r.textContent?.includes("Qwen3 8B"))!;
    // Author, parameters, architecture, quantization and license were five
    // separate table columns; they all live on the row's second line now.
    expect(row).toHaveTextContent("Alibaba");
    expect(row).toHaveTextContent("8B");
    expect(row).toHaveTextContent("Dense");
    expect(row).toHaveTextContent("Q4_K_M");
    expect(row).toHaveTextContent("Apache 2.0");
  });

  describe("the curated picker", () => {
    it("is a radiogroup with one stop per curated model, ordered fast to smart", () => {
      render(<Library />);
      const group = screen.getByRole("radiogroup");
      const stops = within(group).getAllByRole("radio");
      expect(stops.map((s) => s.textContent)).toEqual([
        expect.stringContaining("Gemma 4 E2B"),
        expect.stringContaining("Gemma 4 E4B"),
      ]);
    });

    it("moves the selection with the arrow keys and keeps exactly one stop checked", async () => {
      const user = userEvent.setup();
      render(<Library />);
      const stops = within(screen.getByRole("radiogroup")).getAllByRole("radio");

      // Nothing is recommended in this fixture, so the default lands on the
      // first stop (the middle of a 2-stop dial rounds down).
      expect(stops[0]).toBeChecked();

      stops[0].focus();
      await user.keyboard("{ArrowRight}");
      expect(stops[1]).toBeChecked();
      expect(stops[0]).not.toBeChecked();

      await user.keyboard("{ArrowLeft}");
      expect(stops[0]).toBeChecked();
    });

    it("roves the tab stop so the dial is a single stop in the tab order", async () => {
      render(<Library />);
      const stops = within(screen.getByRole("radiogroup")).getAllByRole("radio");
      expect(stops[0]).toHaveAttribute("tabindex", "0");
      expect(stops[1]).toHaveAttribute("tabindex", "-1");
    });

    it("shows the selected model's card below it", async () => {
      const user = userEvent.setup();
      render(<Library />);
      const stops = within(screen.getByRole("radiogroup")).getAllByRole("radio");

      expect(screen.getByRole("heading", { level: 2, name: "Gemma 4 E2B" })).toBeInTheDocument();
      await user.click(stops[1]);
      expect(screen.getByRole("heading", { level: 2, name: "Gemma 4 E4B" })).toBeInTheDocument();
    });
  });

  describe("the list toolbar", () => {
    it("narrows the list as you search, by name and by author", async () => {
      const user = userEvent.setup();
      render(<Library />);
      const search = screen.getByRole("textbox", { name: /search models/i });

      await user.type(search, "qwen");
      expect(modelRows()).toHaveLength(1);
      expect(modelRows()[0]).toHaveTextContent("Qwen3 8B");

      await user.clear(search);
      await user.type(search, "google");
      expect(modelRows()).toHaveLength(2);
    });

    it("ignores accents and case, so a French query matches", async () => {
      const user = userEvent.setup();
      seed([model({ id: "accented", name: "Modèle Français", source: "huggingFace" })]);
      render(<Library />);

      await user.type(screen.getByRole("textbox", { name: /search models/i }), "modele");
      expect(modelRows()).toHaveLength(1);
    });

    it("narrows the list with the filter chips", async () => {
      const user = userEvent.setup();
      render(<Library />);

      await user.click(screen.getByRole("button", { name: "Installed" }));
      expect(modelRows()).toHaveLength(1);
      expect(modelRows()[0]).toHaveTextContent("Gemma 4 E4B");

      await user.click(screen.getByRole("button", { name: "All" }));
      expect(modelRows()).toHaveLength(3);
    });

    it("offers a way back when a filter leaves nothing, rather than a blank section", async () => {
      const user = userEvent.setup();
      render(<Library />);

      await user.type(screen.getByRole("textbox", { name: /search models/i }), "nothing-matches");
      expect(screen.queryByRole("list", { name: "" })).toBeNull();

      const empty = screen.getByRole("status");
      expect(empty).toHaveTextContent(/no models match/i);
      await user.click(within(empty).getByRole("button", { name: /clear search and filters/i }));
      expect(modelRows()).toHaveLength(3);
    });

    it("stays hidden for a stock install, where there is nothing to navigate", () => {
      seed(MODELS.filter((m) => m.spec.curated));
      render(<Library />);
      expect(screen.queryByRole("textbox", { name: /search models/i })).toBeNull();
    });
  });

  describe("the row overflow menu", () => {
    it("names the model it acts on, so repeated actions are distinguishable", () => {
      render(<Library />);
      expect(
        screen.getByRole("button", { name: "More actions for Gemma 4 E4B" }),
      ).toBeInTheDocument();
      // Nothing destructive is possible for a model that isn't downloaded, so
      // it gets no menu at all rather than an empty one.
      expect(screen.queryByRole("button", { name: "More actions for Qwen3 8B" })).toBeNull();
    });

    it("opens, closes on Escape, and returns focus to its trigger", async () => {
      const user = userEvent.setup();
      render(<Library />);
      const trigger = screen.getByRole("button", { name: "More actions for Gemma 4 E4B" });

      expect(trigger).toHaveAttribute("aria-expanded", "false");
      await user.click(trigger);
      expect(trigger).toHaveAttribute("aria-expanded", "true");
      expect(screen.getByRole("button", { name: "Remove Gemma 4 E4B" })).toBeInTheDocument();

      await user.keyboard("{Escape}");
      expect(trigger).toHaveAttribute("aria-expanded", "false");
      expect(trigger).toHaveFocus();
    });

    it("confirms a removal in a dialog rather than inline, so the row cannot change height", async () => {
      const user = userEvent.setup();
      render(<Library />);

      await user.click(screen.getByRole("button", { name: "More actions for Gemma 4 E4B" }));
      await user.click(screen.getByRole("button", { name: "Remove Gemma 4 E4B" }));

      const dialog = screen.getByRole("alertdialog");
      expect(dialog).toHaveTextContent(/Remove Gemma 4 E4B\?/);
    });
  });

  describe("a download in flight", () => {
    beforeEach(() => {
      useStore.setState({
        downloads: {
          "qwen-hf": {
            receivedBytes: 1024 ** 3,
            totalBytes: 4 * 1024 ** 3,
            bytesPerSec: 5 * 1024 ** 2,
            etaSeconds: 600,
          },
        },
      });
    });

    it("reports progress on the row's own second line, so the row cannot grow", () => {
      render(<Library />);
      const row = modelRows().find((r) => r.textContent?.includes("Qwen3 8B"))!;
      // The bar is absolutely positioned on the row's bottom edge; the
      // byte counter replaces the spec line rather than adding a third one.
      expect(
        within(row).getByRole("progressbar", { name: "Downloading Qwen3 8B" }),
      ).toBeInTheDocument();
      expect(row).toHaveTextContent("1 GB of 4 GB");
      expect(row.querySelectorAll(".model-row-meta")).toHaveLength(1);
      // The spec line is gone while downloading, not stacked underneath.
      expect(row).not.toHaveTextContent("Dense");
    });

    it("offers cancel in the overflow menu instead of a second inline button", async () => {
      const user = userEvent.setup();
      render(<Library />);
      // Within this row, the download button is gone while it's in flight —
      // other rows and the spotlight card still have their own.
      const row = modelRows().find((r) => r.textContent?.includes("Qwen3 8B"))!;
      expect(within(row).queryByRole("button", { name: /download · /i })).toBeNull();
      await user.click(screen.getByRole("button", { name: "More actions for Qwen3 8B" }));
      expect(
        screen.getByRole("button", { name: "Cancel downloading Qwen3 8B" }),
      ).toBeInTheDocument();
    });
  });

  it("announces a failed download on the row and keeps a way to retry", () => {
    // `downloadErrors` outlives the progress entry — it is cleared only by
    // the next attempt — so the row has to have somewhere to put it.
    useStore.setState({ downloadErrors: { "qwen-hf": "Network unreachable" } });
    render(<Library />);
    const row = modelRows().find((r) => r.textContent?.includes("Qwen3 8B"))!;
    const alert = within(row).getByRole("alert");
    expect(alert).toHaveTextContent("Network unreachable");
    expect(within(row).getByRole("button", { name: /download · /i })).toBeInTheDocument();
  });

  describe("the fit verdict", () => {
    it("explains itself on hover and to a screen reader, not as text on the row", () => {
      render(<Library />);
      const row = modelRows().find((r) => r.textContent?.includes("Gemma 4 E4B"))!;
      const fit = row.querySelector(".model-fit")!;
      // The verdict and its explanation both survive — they just aren't 145px
      // of visible text on every row any more.
      expect(fit).toHaveAttribute("title", expect.stringContaining("Perfect fit"));
      expect(fit).toHaveAttribute("title", expect.stringContaining("memory to spare"));
      expect(fit).toHaveTextContent("Perfect fit");
      expect(fit.querySelector("svg")).toBeInTheDocument();
    });

    it("marks a perfect fit with an extra bolt, so it reads as faster than merely good", () => {
      render(<Library />);
      const glyphs = (name: string) => {
        const row = modelRows().find((r) => r.textContent?.includes(name))!;
        return row.querySelectorAll(".model-fit svg").length;
      };
      // Gemma 4 E4B is a perfect fit here, Qwen3 8B only a good one. Both
      // share the check; the bolt is what separates them.
      expect(glyphs("Gemma 4 E4B")).toBe(2);
      expect(glyphs("Qwen3 8B")).toBe(1);
      const row = modelRows().find((r) => r.textContent?.includes("Gemma 4 E4B"))!;
      expect(row.querySelector(".model-fit-bolt")).toBeInTheDocument();
    });

    it("marks both doesn't-fit levels with the same red cross, and separates them in words", () => {
      // The icon set carries exactly two distinctions: fits or doesn't, and
      // fastest or not. "Good fit"/"A bit snug" share the check and differ by
      // colour; "Slow — runs from disk"/"Too big" share the cross and are
      // visually identical, so the wording is the only thing telling them
      // apart — which is why every indicator carries it.
      seed([
        // Non-catalog, because the list deliberately excludes non-curated
        // catalog entries — a plain `spec()` would render an empty list.
        model({ id: "disk", name: "Disk Model", source: "huggingFace" }, { fit: "diskSlow" }),
        model(
          { id: "big", name: "Big Model", source: "huggingFace" },
          { fit: "tooBig", status: INSTALLED },
        ),
      ]);
      render(<Library />);
      const fitOf = (name: string) =>
        modelRows().find((r) => r.textContent?.includes(name))!.querySelector(".model-fit")!;

      const disk = fitOf("Disk Model");
      const big = fitOf("Big Model");
      // Same glyph, same tone, one each — no qualifier on either.
      expect(disk.querySelector("svg")!.innerHTML).toEqual(big.querySelector("svg")!.innerHTML);
      expect(disk).toHaveClass("is-bad");
      expect(big).toHaveClass("is-bad");
      expect(disk.querySelectorAll("svg")).toHaveLength(1);
      expect(big.querySelectorAll("svg")).toHaveLength(1);
      // The wording is where the difference lives.
      expect(disk.getAttribute("title")).not.toEqual(big.getAttribute("title"));
      expect(disk.getAttribute("title")).toMatch(/can still be used/i);
      expect(big.getAttribute("title")).toMatch(/more memory than this Mac has/i);
    });

    it("says a disk-streamed model doesn't fit in memory but is still usable", () => {
      seed([model({ id: "disk", name: "Disk Model", source: "huggingFace" }, { fit: "diskSlow" })]);
      render(<Library />);
      const fit = modelRows()[0].querySelector(".model-fit")!;
      const title = fit.getAttribute("title")!;
      expect(title).toMatch(/doesn't fit in your memory/i);
      expect(title).toMatch(/can still be used/i);
      expect(title).toMatch(/far slower/i);
    });

    it("explains every level in a legend beside the heading", async () => {
      const user = userEvent.setup();
      render(<Library />);
      await user.click(screen.getByRole("button", { name: /what do the fit icons mean/i }));

      const legend = screen.getByRole("dialog");
      for (const label of [
        "Perfect fit",
        "Good fit",
        "A bit snug",
        "Runs slowly from disk",
        "Too big for this Mac",
      ]) {
        expect(legend).toHaveTextContent(label);
      }
      expect(legend.querySelectorAll(".fit-legend li")).toHaveLength(5);
      // The bolt appears in the legend too, so its meaning is discoverable.
      expect(legend.querySelector(".model-fit-bolt")).toBeInTheDocument();
    });

    it("keeps the full wording on the spotlight card, where there is room", () => {
      render(<Library />);
      // The card shows whichever model the dial has selected — Gemma 4 E2B by
      // default here, a "good" fit — and still spells its verdict out.
      const card = document.querySelector(".model-card")!;
      expect(card).toHaveTextContent("Gemma 4 E2B");
      expect(card.querySelector(".badge")).toHaveTextContent("Good fit");
    });
  });

  describe("the quantization explainer", () => {
    beforeEach(() => {
      seed([
        ...MODELS,
        model({ id: "repo-q4", name: "Repo Q4", groupId: "org/repo", source: "huggingFace" }),
        model({ id: "repo-q8", name: "Repo Q8", groupId: "org/repo", source: "huggingFace" }),
      ]);
    });

    it("sits next to the word it explains and opens in plain language", async () => {
      const user = userEvent.setup();
      render(<Library />);

      const trigger = screen.getByRole("button", { name: /what is a quantization/i });
      await user.click(trigger);

      const pop = screen.getByRole("dialog");
      // Beginner-facing: an analogy first, jargon decoded second — no
      // mention of bits, weights or precision.
      expect(pop).toHaveTextContent(/saving a photo at a smaller file size/i);
      expect(pop).toHaveTextContent(/Q4 is squeezed more than Q8/i);
    });

    it("closes on Escape and gives focus back to the icon", async () => {
      const user = userEvent.setup();
      render(<Library />);
      const trigger = screen.getByRole("button", { name: /what is a quantization/i });

      await user.click(trigger);
      expect(trigger).toHaveAttribute("aria-expanded", "true");
      await user.keyboard("{Escape}");
      expect(screen.queryByRole("dialog")).toBeNull();
      expect(trigger).toHaveFocus();
    });
  });

  it("'Go to chat' on the model in use closes the More dialog onto the chat", async () => {
    const user = userEvent.setup();
    useStore.setState({
      engine: {
        type: "running",
        data: { modelId: "gemma-e4b", port: 1, diskSlow: false, autoUnloadAt: null },
      },
      moreDialogPage: "library",
      view: "improve",
    });
    render(<Library />);

    await user.click(screen.getAllByRole("button", { name: "Go to chat" })[0]);

    expect(useStore.getState().moreDialogPage).toBeNull();
    expect(useStore.getState().view).toBe("chat");
  });

  it("puts memory and the Finder shortcut on one line", async () => {
    const user = userEvent.setup();
    render(<Library />);
    expect(screen.getByText(/available memory: 12 GB/i)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /show downloads in finder/i }));
    expect(openModelsFolder).toHaveBeenCalledOnce();
  });

  it("hides 'Add your own model' until Settings turns it on, then shows it last", () => {
    const { container } = render(<Library />);
    expect(screen.queryByRole("heading", { name: "Add your own model" })).toBeNull();

    act(() =>
      useStore.setState({
        settings: { autoLoadOnStartup: true, customModelsEnabled: true } as unknown as Settings,
      }),
    );
    const heading = screen.getByRole("heading", { level: 2, name: "Add your own model" });
    expect(container.querySelector(".library-col")?.lastElementChild).toContainElement(heading);
  });
});
