class PatternExamples
  def self.fee
    rule = {threshold: 0, fixed: 30, rate: "0.0"}
    case rule
    in threshold: (1..)
      "threshold"
    in fixed: (1..), rate: /^$|0\.0/
      "fixed"
    else
      "variable"
    end
  end

  def self.result
    case {code: :ok, data: {user: false}}
    in {code: :ok, data: {user:}}
      user
    in {code: :stale | :invalid}
      true
    else
      nil
    end
  end

  def self.guarded
    case [3, 4, 5]
    in [Integer => first, *middle, last] if first > 0
      first + last + middle.length
    else
      0
    end
  end

  def self.exhaustive
    case {missing: nil}
    in {present:}
      present
    end
  end

  def self.custom
    case PatternResult.new
    in {code: :ok, data: {user:}}
      user
    else
      true
    end
  end

  def self.subject(log)
    log << :called
    {ok: false}
  end

  def self.once
    log = []
    result = case subject(log)
    in {ok:}
      ok
    else
      true
    end
    [result, log.length]
  end

  def self.unmatched
    case [:bad]
    in [:ok]
      1
    end
  end

end
