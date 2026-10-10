import json
import sys
from pathlib import Path

import numpy as np
import torch
from qwen_tts import Qwen3TTSModel

HERE = Path(__file__).parent


def main():
    spec = json.loads(Path(sys.argv[1]).read_text())
    voice, todo = spec["voice"], [j for j in spec["jobs"] if not j["cached"]]
    model = Qwen3TTSModel.from_pretrained(voice["model"], device_map="cuda:0", dtype=torch.bfloat16, attn_implementation="sdpa")
    prompt = model.create_voice_clone_prompt(ref_audio=str(HERE / voice["reference"]), ref_text=voice["referenceText"])
    for i, job in enumerate(todo, 1):
        torch.manual_seed(job["seed"])
        np.random.seed(job["seed"])
        wavs, sr = model.generate_voice_clone(text=job["text"], language=voice["language"], voice_clone_prompt=prompt,
                                              **voice["sampling"])
        if sr != voice["sampleRate"]:
            raise SystemExit(f"model returned {sr} Hz audio; VOICE.sampleRate says {voice['sampleRate']}")
        x = np.asarray(wavs[0], dtype=np.float32)
        (HERE / ".cache" / f"{job['key']}.f32").write_bytes(x.tobytes())
        print(f"[{i}/{len(todo)}] {len(x) / sr:5.2f}s  {job['text']}", flush=True)


if __name__ == "__main__":
    main()
