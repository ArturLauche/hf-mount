import { describe, expect, it } from "vitest";
import { hubUrl, idProblem, parseSourceInput, revisionProblem, sourceLine, subfolderProblem } from "./source";

describe("parseSourceInput", () => {
  it("accepts plain ids", () => {
    expect(parseSourceInput(" openai-community/gpt2 ")).toEqual({ id: "openai-community/gpt2" });
    expect(parseSourceInput("gpt2").id).toBe("gpt2");
  });

  it("reads the type from prefixes", () => {
    expect(parseSourceInput("datasets/HuggingFaceFW/fineweb")).toMatchObject({ source_type: "dataset", id: "HuggingFaceFW/fineweb" });
    expect(parseSourceInput("hf://buckets/alice/runs")).toMatchObject({ source_type: "bucket", id: "alice/runs" });
    expect(parseSourceInput("spaces/gradio/hello_world")).toMatchObject({ source_type: "space" });
  });

  it("splits Hub browser URLs into id, revision and folder", () => {
    expect(parseSourceInput("https://huggingface.co/datasets/HuggingFaceFW/fineweb-edu/tree/main/sample/10BT")).toEqual({
      source_type: "dataset",
      id: "HuggingFaceFW/fineweb-edu",
      revision: "main",
      subfolder: "sample/10BT",
    });
    expect(parseSourceInput("https://huggingface.co/openai-community/gpt2/blob/v1.0/onnx?download=true")).toEqual({
      id: "openai-community/gpt2",
      revision: "v1.0",
      subfolder: "onnx",
    });
    expect(parseSourceInput("huggingface.co/openai-community/gpt2#readme").id).toBe("openai-community/gpt2");
  });

  it("decodes encoded revisions", () => {
    expect(parseSourceInput("https://huggingface.co/org/repo/tree/refs%2Fpr%2F1").revision).toBe("refs/pr/1");
  });
});

describe("validation", () => {
  it("checks ids like the engine", () => {
    expect(idProblem("model", "openai-community/gpt2")).toBeNull();
    expect(idProblem("model", "gpt2")).toBeNull();
    expect(idProblem("model", "")).toMatch(/namespace\/name/);
    expect(idProblem("model", "a/b/c")).toMatch(/namespace\/name/);
    expect(idProblem("bucket", "alice")).toMatch(/namespace\/bucket/);
    expect(idProblem("model", "org/-bad")).toMatch(/start or end/);
    expect(idProblem("model", "org/a--b")).toMatch(/--/);
    expect(idProblem("model", "org/../x")).not.toBeNull();
    expect(idProblem("dataset", "org/name with space")).toMatch(/letters, digits/);
  });

  it("checks revisions and folders", () => {
    expect(revisionProblem("")).toBeNull();
    expect(revisionProblem("refs/pr/1")).toBeNull();
    expect(revisionProblem("a..b")).not.toBeNull();
    expect(revisionProblem("main;rm")).not.toBeNull();
    expect(subfolderProblem("onnx/fp16")).toBeNull();
    expect(subfolderProblem("a\\b")).toMatch(/'\/'/);
    expect(subfolderProblem("a/../b")).not.toBeNull();
  });
});

describe("display helpers", () => {
  it("formats sources and links", () => {
    const spec = { source_type: "dataset" as const, id: "org/ds", target: "D:", read_only: true, subfolder: "x/y", revision: null };
    expect(sourceLine(spec)).toBe("org/ds@main · /x/y");
    expect(hubUrl(spec)).toBe("https://huggingface.co/datasets/org/ds");
    expect(sourceLine({ ...spec, source_type: "bucket", subfolder: null })).toBe("org/ds");
  });
});
