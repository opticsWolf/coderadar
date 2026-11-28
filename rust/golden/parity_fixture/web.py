"""Flask routes so `resolve` and framework extraction see one handler."""

from flask import Flask, jsonify

app = Flask(__name__)


@app.route("/health")
def health():
    return jsonify(ok=True)


@app.route("/orders/<int:order_id>")
def order_detail(order_id: int):
    return jsonify(order_id=order_id)
