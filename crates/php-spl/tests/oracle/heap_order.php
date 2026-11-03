<?php
// Equal priorities: PHP's heap is not stable; order must match exactly.
$pq = new SplPriorityQueue();
foreach (['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j'] as $i => $v) {
    $pq->insert($v, $i % 3);
}
$pq->setExtractFlags(SplPriorityQueue::EXTR_BOTH);
while ($pq->valid()) {
    $e = $pq->extract();
    echo $e['data'], ':', $e['priority'], "\n";
}
$pq = new SplPriorityQueue();
$pq->insert('x', [1, 2]);
$pq->insert('y', [1, 3]);
$pq->insert('z', [0, 9]);
$pq->insert('w', 5);
foreach ($pq as $k => $v) { echo "$k=$v\n"; }
$h = new SplMinHeap();
foreach ([5, 3, 8, 1, 9, 2, 7, 3, 3, 0, -1, 10] as $v) $h->insert($v);
echo implode(',', iterator_to_array($h, false)), "\n";
echo count($h), "\n";
$h = new SplMaxHeap();
foreach (["b", "a", "10", "9", 9.5, true, null] as $v) $h->insert($v);
var_dump($h);
while (!$h->isEmpty()) var_dump($h->extract());
try { $h->top(); } catch (RuntimeException $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
class BadHeap extends SplHeap {
    public $n = 0;
    protected function compare($a, $b): int {
        if (++$this->n == 4) throw new Exception("boom");
        return $a <=> $b;
    }
}
$b = new BadHeap();
try { foreach ([1, 2, 3, 4, 5] as $v) $b->insert($v); } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump($b->isCorrupted(), count($b));
try { $b->insert(6); } catch (RuntimeException $e) { echo $e->getMessage(), "\n"; }
$b->recoverFromCorruption();
var_dump($b->isCorrupted());
$pq = new SplPriorityQueue();
$pq->insert(1, 2);
try { $pq->setExtractFlags(0); } catch (RuntimeException $e) { echo $e->getMessage(), "\n"; }
var_dump($pq->setExtractFlags(7));
