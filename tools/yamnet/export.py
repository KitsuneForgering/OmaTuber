"""Export YAMNet's CNN (log-mel patches -> 521 scores) to ONNX.

The log-mel front end stays out of the ONNX graph; compare.py computes it in
NumPy. As a check, the script runs the original TensorFlow model on a test
clip, compares its scores with the ONNX ones, and saves the reference log-mel.

usage: export.py <yamnet-src-dir> <test.wav 16 kHz mono 16-bit> <out-dir>

<yamnet-src-dir> holds yamnet.py, params.py, features.py, yamnet_class_map.csv
from tensorflow/models research/audioset/yamnet and the weights from
https://storage.googleapis.com/audioset/yamnet.h5. Tested with Python 3.11,
tensorflow-cpu 2.15.1, tf_keras 2.15.1, tf2onnx 1.16.1, onnxruntime 1.26.
"""
import json, sys, wave
import numpy as np

src, wav_path, out = sys.argv[1:4]
sys.path.insert(0, src)
import tensorflow as tf
import tf_keras
import tf2onnx
import params as yparams
import yamnet as ymodel

p = yparams.Params()
full = ymodel.yamnet_frames_model(p)
full.load_weights(f"{src}/yamnet.h5")

inp = tf_keras.layers.Input(shape=(p.patch_frames, p.patch_bands), name="patches")
pred, _ = ymodel.yamnet(inp, p)
cnn = tf_keras.Model(inp, pred)
src_layers = [l for l in full.layers if l.get_weights()]
dst_layers = [l for l in cnn.layers if l.get_weights()]
assert len(src_layers) == len(dst_layers), (len(src_layers), len(dst_layers))
for a, b in zip(src_layers, dst_layers):
    b.set_weights(a.get_weights())
copied = len(dst_layers)
print("layers with weights copied:", copied)

spec = (tf.TensorSpec([None, p.patch_frames, p.patch_bands], tf.float32, name="patches"),)
tf2onnx.convert.from_keras(cnn, input_signature=spec, opset=13, output_path=f"{out}/yamnet.onnx")

with wave.open(wav_path) as w:
    assert w.getframerate() == 16000 and w.getnchannels() == 1 and w.getsampwidth() == 2
    pcm = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float32) / 32768.0
scores, _, logmel = full(pcm)
scores, logmel = scores.numpy(), logmel.numpy()

import onnxruntime as ort
frames = tf.signal.frame(logmel, p.patch_frames, p.patch_frames // 2, axis=0).numpy()
sess = ort.InferenceSession(f"{out}/yamnet.onnx")
onnx_scores = sess.run(None, {"patches": frames})[0]
n = min(len(onnx_scores), len(scores))
print("patches:", len(scores), "onnx vs tf max abs diff:", float(np.abs(onnx_scores[:n] - scores[:n]).max()))

np.save(f"{out}/ref_logmel.npy", logmel)
np.save(f"{out}/ref_scores.npy", scores)
names = ymodel.class_names(f"{src}/yamnet_class_map.csv")
top = [[(names[i], round(float(s[i]), 3)) for i in np.argsort(-s)[:3]] for s in scores]
print(json.dumps(top, ensure_ascii=False))
