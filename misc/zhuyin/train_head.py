"""Adds zhuyin (bopomofo) to the PP-OCRv5 mobile recognition model without retraining the network.

The model ends in one linear layer: 120 features per frame -> one score per class. Everything before it stays frozen
and the existing class columns stay as they are. Only new columns, one per zhuyin symbol and tone mark, are learned.

They are trained on rendered lines. Where the frozen model reads something inside a zhuyin glyph, that frame gets the
zhuyin class as target; every other frame keeps the class the model already gives it, so the new columns learn to stay
quiet on ordinary text.

The result is the same model file with a second output, `zhuyin`: the class probabilities with the new classes put in
before the last class (the space). The first output is untouched.

Runs on a CPU. See README.md next to this file for the command and the results.
"""
import argparse
import pickle
import random
import re
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
import torch
from fontTools.ttLib import TTFont
from onnx import TensorProto, helper, numpy_helper
from PIL import Image, ImageDraw, ImageFilter, ImageFont

# The tensors of the last layer in the model file
FEATURES, LOGITS = 'p2o.pd_op.transpose.8.0', 'p2o.pd_op.add.100.0'
WEIGHT, BIAS = 'linear_8.w_0', 'linear_8.b_0'

INITIALS = 'ㄅㄆㄇㄈㄉㄊㄋㄌㄍㄎㄏㄐㄑㄒㄓㄔㄕㄖㄗㄘㄙ'
MEDIALS = 'ㄧㄨㄩ'
FINALS = 'ㄚㄛㄜㄝㄞㄟㄠㄡㄢㄣㄤㄥㄦ'
TONES = 'ˊˇˋ˙'
ZHUYIN = INITIALS + MEDIALS + FINALS + TONES
# Characters the model gives for zhuyin glyphs today, and others of similar shape
LOOKALIKES = 'クタロ分去了力人久万厂山リヘへ出テイ屍尸戸日アちムさせ历牙另入幺又ヌ马ウ尢儿ル一レ口刀勹夂冂匚丂巜丩彳卩厶丫乁' \
             'CTYXLPcv∠×—-^>7'
INKS = [(0, 0, 0), (20, 20, 20), (30, 60, 160), (110, 40, 150), (200, 30, 120), (180, 30, 30), (70, 70, 70)]
# Small sizes matter: a line of body text on a page is around 25 px tall when it is cut out, and gets scaled up
FONT_SIZES = (14, 18, 24, 32, 40)
MAX_CHARS = 40


class Font:
    """A font in the sizes lines are rendered in, and the characters it has."""

    def __init__(self, path):
        self.sizes = [ImageFont.truetype(path, size) for size in FONT_SIZES]
        self.chars = set(map(chr, TTFont(path, lazy=True).getBestCmap())) | {' '}

    def covers(self, text):
        return set(text) <= self.chars


@dataclass
class Line:
    """A rendered line as the frozen model sees it."""
    text: str
    spans: list  # (character, left, right): the horizontal extent of each glyph as a share of the width
    features: np.ndarray  # [frames, 120]
    fired: np.ndarray  # [frames], the class the frozen model gives each frame


def syllable(rng):
    """A zhuyin syllable. Not always a real one, which does not matter for telling glyphs apart."""
    s = ''
    if rng.random() < 0.8:
        s += rng.choice(INITIALS)
    if rng.random() < 0.4:
        s += rng.choice(MEDIALS)
    if rng.random() < 0.8 or not s:
        s += rng.choice(FINALS)
    r = rng.random()
    if r < 0.6:
        s += rng.choice(TONES[:3])
    elif r < 0.7:
        s = TONES[3] + s  # the neutral tone dot is written in front
    return s


def zhuyin_text(rng, corpus):
    """The text of a line with zhuyin in it."""
    kind = rng.random()
    if kind < 0.5:  # syllables
        return ' '.join(syllable(rng) for _ in range(rng.randint(1, 7)))
    if kind < 0.65:  # a practice row: one symbol or syllable repeated, or the symbols in their order
        if rng.random() < 0.5:
            start = rng.randrange(len(ZHUYIN) - 4)
            return rng.choice(['', ' ']).join(ZHUYIN[start:start + rng.randint(3, 10)])
        unit = rng.choice([rng.choice(ZHUYIN[:37]), syllable(rng)])
        return rng.choice(['', ' ', '  ']).join([unit] * rng.randint(2, 8))
    # zhuyin mixed into ordinary text
    words = rng.choice(corpus).split(' ')
    if len(words) == 1:  # text without spaces: cut it into pieces
        text = words[0]
        cuts = sorted(rng.sample(range(len(text) + 1), min(3, len(text) + 1)))
        words = [text[a:b] for a, b in zip([0] + cuts, cuts + [len(text)])]
    for _ in range(rng.randint(1, 3)):
        words.insert(rng.randint(0, len(words)), syllable(rng))
    return rng.choice(['', ' ']).join(w for w in words if w)[:MAX_CHARS]


def other_text(rng, corpus):
    """The text of a line without zhuyin: a sentence, or characters that look like zhuyin."""
    if rng.random() < 0.2:
        return ''.join(rng.choice(LOOKALIKES) for _ in range(rng.randint(1, 10)))
    return rng.choice(corpus)[:MAX_CHARS]


def sweep_text(rng, fonts, dictionary):
    """Characters from all over the dictionary, as many as a font has. No class is left unseen this way."""
    font = rng.choice(fonts)
    pool = [c for c in dictionary if len(c) == 1 and c.strip() and c in font.chars]
    return ''.join(rng.choice(pool) for _ in range(rng.randint(8, MAX_CHARS)))


def read_corpus(text_dir):
    """Ordinary sentences in many languages: the source and translated strings of gettext `.po` files."""
    lines = set()
    for path in sorted(Path(text_dir).glob('*.po')):
        for string in re.findall(r'^msg(?:id|str) "(.+)"$', path.read_text(encoding='utf-8'), flags=re.M):
            string = re.sub(r'<[^>]+>|\\[nt"]|_|%\w|\{\w*\}', '', string).strip()
            if 2 <= len(string) and not set(string) & set(ZHUYIN):
                lines.add(string)
    return sorted(lines)


def render(text, font, rng):
    """Renders a line the way a detected line crop looks. Returns the image and the horizontal extent of each glyph."""
    size = font.size
    pad_x, pad_y = rng.randint(2, size // 2), rng.randint(2, size // 2)
    left, top, right, bottom = font.getbbox(text)
    width, height = max(1, right - left) + 2 * pad_x, max(1, bottom - top) + 2 * pad_y
    background = rng.choice([(255, 255, 255)] * 4 + [(250, 248, 240), (235, 240, 250), (245, 235, 235)])
    image = Image.new('RGB', (width, height), background)
    ImageDraw.Draw(image).text((pad_x - left, pad_y - top), text, font=font, fill=rng.choice(INKS),
                               stroke_width=rng.choice([0, 0, 0, 1]))
    if rng.random() < 0.3:
        image = image.filter(ImageFilter.GaussianBlur(rng.uniform(0.3, 1.0)))
    advances = [font.getlength(text[:i]) for i in range(len(text) + 1)]
    spans = [(ch, (pad_x - left + advances[i]) / width, (pad_x - left + advances[i + 1]) / width)
             for i, ch in enumerate(text)]
    return image, spans


class Model:
    """The frozen recognition model, giving the class probabilities and the features that feed the last layer."""

    def __init__(self, path):
        model = onnx.load(path)
        model.graph.output.append(helper.make_tensor_value_info(FEATURES, TensorProto.FLOAT, None))
        options = ort.SessionOptions()
        options.intra_op_num_threads = 2
        self.session = ort.InferenceSession(model.SerializeToString(), options, providers=['CPUExecutionProvider'])

    def run(self, image):
        width = max(8, round(48 * image.width / image.height))
        pixels = np.asarray(image.convert('RGB').resize((width, 48), Image.BILINEAR), dtype=np.float32)[:, :, ::-1]
        x = np.ascontiguousarray(((pixels / 255.0 - 0.5) / 0.5).transpose(2, 0, 1)[None])
        probabilities, features = self.session.run(None, {'x': x})
        return probabilities[0], features[0]


def collect(model, fonts, texts, rng):
    """Renders the texts and returns them as the frozen model sees them.

    Each text is rendered with a font that has all of its characters. Texts that no font can render are left out,
    as missing glyphs come out as boxes and say nothing about how text is read.
    """
    lines = []
    for text in texts:
        usable = [font for font in fonts if font.covers(text)]
        if not usable or not text.strip():
            continue
        image, spans = render(text, rng.choice(rng.choice(usable).sizes), rng)
        probabilities, features = model.run(image)
        lines.append(Line(text, spans, features, probabilities.argmax(axis=1)))
    return lines


def frame_targets(line, new_class):
    """The target class and the training weight of each frame of a line.

    Frames keep the class the frozen model fires. Inside a zhuyin glyph, the frames where it fires something get the
    zhuyin class. A zhuyin glyph the model fires nothing for is left alone: teaching the new classes to fire on blank
    frames makes them fire on blank frames of ordinary text too. The other frames inside a zhuyin glyph do not count:
    next to a frame of the same class they merge with it when the line is decoded.
    """
    n_frames = len(line.fired)
    # Frames where the model reads a character count more than the many blank ones: they are the ones a zhuyin
    # class must not take over
    targets, weights = line.fired.copy(), np.where(line.fired != 0, 3.0, 1.0).astype(np.float32)
    centers = (np.arange(n_frames) + 0.5) / n_frames
    for ch, x0, x1 in line.spans:
        if ch in new_class:
            inside = np.flatnonzero((centers >= x0) & (centers < x1))
            spikes = inside[line.fired[inside] != 0]
            weights[inside] = 0.0
            targets[spikes], weights[spikes] = new_class[ch], 4.0
    return targets, weights


def train(lines, weight, bias, new_class, epochs, seed):
    """Learns the columns of the new classes. `weight` is [120, n_old], `bias` is [n_old], both stay fixed.

    All the loss needs from the fixed classes is two numbers per frame: the log-sum of their scores, and the score of
    the target where the target is one of them. That is the best score, as such a frame keeps the class it fires.
    """
    torch.manual_seed(seed)
    n_old = weight.shape[1]
    targets = [frame_targets(line, new_class) for line in lines]
    x = torch.from_numpy(np.concatenate([line.features for line in lines]))
    y = torch.from_numpy(np.concatenate([t for t, _ in targets])).long()
    frame_weight = torch.from_numpy(np.concatenate([w for _, w in targets]))
    w_old, b_old = torch.from_numpy(weight.copy()), torch.from_numpy(bias.copy())
    # In chunks that are reduced right away: all frames against all classes at once does not fit into memory
    log_sum_old, best_old = [], []
    for i in range(0, len(x), 4096):
        scores = x[i:i + 4096] @ w_old + b_old
        log_sum_old.append(torch.logsumexp(scores, dim=1))
        best_old.append(scores.max(dim=1).values)
    log_sum_old, best_old = torch.cat(log_sum_old), torch.cat(best_old)

    is_new = y >= n_old
    new_index = (y - n_old).clamp(min=0)[:, None]
    w_new = torch.zeros(weight.shape[0], len(new_class), requires_grad=True)
    b_new = torch.zeros(len(new_class), requires_grad=True)
    optimizer = torch.optim.Adam([w_new, b_new], lr=0.01)
    print(f'{len(lines)} lines, {len(x)} frames, {int(is_new.sum())} of them zhuyin', flush=True)
    for epoch in range(epochs):
        order, total = torch.randperm(len(x)), 0.0
        for batch in order.split(2048):
            scores = x[batch] @ w_new + b_new
            log_sum = torch.logaddexp(log_sum_old[batch], torch.logsumexp(scores, dim=1))
            target = torch.where(is_new[batch], scores.gather(1, new_index[batch])[:, 0], best_old[batch])
            loss = ((log_sum - target) * frame_weight[batch]).mean()
            optimizer.zero_grad()
            loss.backward()
            optimizer.step()
            total += loss.item() * len(batch)
        print(f'epoch {epoch + 1}: loss {total / len(x):.4f}', flush=True)
    return w_new.detach().numpy(), b_new.detach().numpy()


def decode(fired, classes):
    """The text of a line: the class of each frame, runs merged, blanks dropped. Spaces are left out."""
    keep = (fired != 0) & (fired != np.concatenate([[0], fired[:-1]]))
    return ''.join(classes[c] for c in fired[keep]).replace(' ', '')


def edit_distance(a, b):
    row = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        previous, row[0] = row[0], i
        for j, cb in enumerate(b, 1):
            previous, row[j] = row[j], min(row[j] + 1, row[j - 1] + 1, previous + (ca != cb))
    return row[-1]


def evaluate(lines, weight, bias, w_new, b_new, classes, new_class):
    """What the new columns change on the lines.

    The recogniser keeps up to five readings per character, each with at least 1% probability, and search matches
    on any of them. So besides the first reading it matters whether the right one is still among those.
    """
    n_old = weight.shape[1]
    errors_old = errors_new = truth_chars = 0
    symbols = symbols_first = symbols_kept = 0
    characters = taken = taken_but_kept = 0
    for line in lines:
        old = line.features @ weight + bias
        new = line.features @ w_new + b_new
        best_old = old.max(axis=1)
        total = np.logaddexp(np.log(np.exp(old - best_old[:, None]).sum(axis=1)) + best_old,
                             np.log(np.exp(new).sum(axis=1)))
        fifth_old = np.partition(old[:, 1:], -5, axis=1)[:, -5]
        wins = new.max(axis=1) > best_old
        fired_new = np.where(wins, n_old + new.argmax(axis=1), line.fired)

        truth = line.text.replace(' ', '')
        errors_old += edit_distance(decode(line.fired, classes), truth)
        errors_new += edit_distance(decode(fired_new, classes), truth)
        truth_chars += len(truth)

        targets, _ = frame_targets(line, new_class)
        for frame in np.flatnonzero(targets >= n_old):
            score = new[frame, targets[frame] - n_old]
            symbols += 1
            symbols_first += fired_new[frame] == targets[frame]
            symbols_kept += (score - total[frame] >= np.log(0.01) and score > fifth_old[frame]
                             and (new[frame] > score).sum() < 5)
        # Characters of ordinary text whose first reading becomes a zhuyin symbol
        ordinary = (line.fired != 0) & (targets < n_old)
        characters += int(ordinary.sum())
        taken += int((ordinary & wins).sum())
        taken_but_kept += int((ordinary & wins & (best_old - total >= np.log(0.01))
                               & ((new > best_old[:, None]).sum(axis=1) <= 4)).sum())
    result = {'lines': len(lines)}
    if symbols:
        result |= {'error rate before': f'{errors_old / truth_chars:.1%}',
                   'error rate after': f'{errors_new / truth_chars:.1%}',
                   'zhuyin symbols read as first reading': f'{symbols_first / symbols:.1%}',
                   'zhuyin symbols among the kept readings': f'{symbols_kept / symbols:.1%}'}
    result |= {'ordinary characters': characters,
               'of them with a zhuyin first reading': f'{taken} ({taken / max(characters, 1):.2%})',
               'of those, old reading no longer kept': taken - taken_but_kept}
    return result


def add_output(graph, w_new, b_new):
    """Adds the `zhuyin` output: the probabilities with the new classes in before the last class, the space."""
    n_old = numpy_helper.to_array(next(i for i in graph.graph.initializer if i.name == BIAS)).shape[0]
    graph.graph.initializer.extend([
        numpy_helper.from_array(w_new.astype(np.float32), 'zhuyin.w'),
        numpy_helper.from_array(b_new.astype(np.float32), 'zhuyin.b'),
        numpy_helper.from_array(np.array([n_old - 1, 1], dtype=np.int64), 'zhuyin.split'),
    ])
    graph.graph.node.extend([
        helper.make_node('MatMul', [FEATURES, 'zhuyin.w'], ['zhuyin.matmul']),
        helper.make_node('Add', ['zhuyin.matmul', 'zhuyin.b'], ['zhuyin.scores']),
        helper.make_node('Split', [LOGITS, 'zhuyin.split'], ['zhuyin.before', 'zhuyin.space'], axis=2),
        helper.make_node('Concat', ['zhuyin.before', 'zhuyin.scores', 'zhuyin.space'], ['zhuyin.logits'], axis=2),
        helper.make_node('Softmax', ['zhuyin.logits'], ['zhuyin'], axis=2),
    ])
    graph.graph.output.append(
        helper.make_tensor_value_info('zhuyin', TensorProto.FLOAT, ['batch', 'frames', n_old + len(b_new)]))
    onnx.checker.check_model(graph)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('--model', required=True, help='The recognition model. It is not written to.')
    parser.add_argument('--dict', required=True)
    parser.add_argument('--fonts', nargs='+', required=True,
                        help='Font files to train with. Lines with zhuyin use the ones that have zhuyin.')
    parser.add_argument('--test-fonts', nargs='+', required=True, help='Font files that are only used for testing.')
    parser.add_argument('--text-dir', required=True, help='A directory of .po files, for lines without zhuyin.')
    parser.add_argument('--out', required=True, help='The directory for the new model file and the zhuyin dictionary.')
    parser.add_argument('--cache', help='A directory to keep the rendered lines in between runs.')
    parser.add_argument('--lines', type=int, default=12000)
    parser.add_argument('--sweep-lines', type=int, default=5000)
    parser.add_argument('--epochs', type=int, default=2,
                        help='Few on purpose: trained longer, the new classes fire more on text they have not seen.')
    parser.add_argument('--margin', type=float, default=1.0,
                        help='How much a zhuyin class has to beat the existing classes by, in score.')
    parser.add_argument('--seed', type=int, default=1)
    args = parser.parse_args()
    rng = random.Random(args.seed)

    dictionary = Path(args.dict).read_text(encoding='utf-8').split('\n')
    dictionary = dictionary[:-1] if dictionary[-1] == '' else dictionary
    assert not set(ZHUYIN) & set(dictionary), 'The dictionary already has zhuyin.'
    graph = onnx.load(args.model)
    initializers = {i.name: i for i in graph.graph.initializer}
    weight, bias = numpy_helper.to_array(initializers[WEIGHT]), numpy_helper.to_array(initializers[BIAS])
    n_old = weight.shape[1]
    assert n_old == len(dictionary) + 2, 'The dictionary does not belong to the model.'
    # While training and testing, the new classes sit after all old ones
    new_class = {ch: n_old + i for i, ch in enumerate(ZHUYIN)}
    classes = [''] + dictionary + [' '] + list(ZHUYIN)

    corpus = read_corpus(args.text_dir)
    rng.shuffle(corpus)
    test_corpus, corpus = corpus[:len(corpus) // 5], corpus[len(corpus) // 5:]
    fonts = [Font(path) for path in args.fonts]
    test_fonts = [Font(path) for path in args.test_fonts]
    with_zhuyin = lambda fonts: [font for font in fonts if font.covers(ZHUYIN)]
    sets = {
        'train': (fonts, lambda: [zhuyin_text(rng, corpus) if rng.random() < 0.4 else other_text(rng, corpus)
                                  for _ in range(args.lines)]),
        'sweep': (fonts, lambda: [sweep_text(rng, with_zhuyin(fonts), dictionary) for _ in range(args.sweep_lines)]),
        'zhuyin, test fonts': (test_fonts, lambda: [zhuyin_text(rng, test_corpus) for _ in range(600)]),
        'other text, test fonts': (test_fonts, lambda: [text[:MAX_CHARS] for text in test_corpus[:2500]]),
        'sweep, test fonts': (test_fonts, lambda: [sweep_text(rng, with_zhuyin(test_fonts), dictionary)
                                                   for _ in range(800)]),
    }
    model = Model(args.model)

    def lines_of(name):
        cache = Path(args.cache) / f'{name}.pickle' if args.cache else None
        if cache and cache.exists():
            return pickle.loads(cache.read_bytes())
        set_fonts, texts = sets[name]
        texts = texts()
        print(f'rendering {name}: {len(texts)} lines ...', flush=True)
        lines = collect(model, set_fonts, texts, rng)
        if cache:
            cache.parent.mkdir(parents=True, exist_ok=True)
            cache.write_bytes(pickle.dumps(lines))
        return lines

    w_new, b_new = train(lines_of('train') + lines_of('sweep'), weight, bias, new_class, args.epochs, args.seed)
    b_new = b_new - args.margin
    for name in list(sets)[2:]:
        print(name, evaluate(lines_of(name), weight, bias, w_new, b_new, classes, new_class), flush=True)

    add_output(graph, w_new, b_new)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    onnx.save(graph, out / Path(args.model).name)
    (out / 'zhuyin_dict.txt').write_text('\n'.join(ZHUYIN) + '\n', encoding='utf-8')
    print(f'wrote {out}: output `zhuyin` with {n_old + len(ZHUYIN)} classes, margin {args.margin}', flush=True)


if __name__ == '__main__':
    main()
