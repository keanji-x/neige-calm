# Your first project

A small portfolio you can change through an ordinary Neige Development Track.
The page contains two inline illustrations and has no external dependencies.
Open `index.html` directly, or view it in a Report HTML block.

Start with your own request, or try:

> 把页面主标题改成“我的作品集”，再增加一张带封面图的项目卡片。

After seeing the result, continue in the same conversation. For example:

> 把新卡片的标题改成“新的旅程”，保留现有布局。

The repository is a starting point, not a simulated response. A real Planner and
workers make the changes; the Report shows the resulting file and verification.
No GitHub repository or PR is needed for this local example.

Check the actual file with Python 3:

```sh
python3 verify.py
python3 verify.py --title '我的作品集'
```

The second command should fail until the heading is changed. These checks cover
page structure and image labels. They do not inspect CSS, JavaScript, network
requests or visual layout. Inspect the rendered page separately.
